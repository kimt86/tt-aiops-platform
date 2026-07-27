//! Isolated read/monitor web service for the scenario subsystem. A SEPARATE axum server
//! (own process/port) — never mounted on the critical dashboard API. Reads scenario.* for
//! monitoring and serves on-demand scenario/emulator downloads (assembled synchronously,
//! LOCAL, zero Oracle). The only write is the kill switch.

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::DateTime;
use serde::Deserialize;
use sqlx::PgPool;

/// Largest window a single on-demand download may assemble (see `download`).
const MAX_WINDOW_DAYS: i64 = 7;

struct AppErr(anyhow::Error);
impl IntoResponse for AppErr {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}
impl<E: Into<anyhow::Error>> From<E> for AppErr {
    fn from(e: E) -> Self {
        AppErr(e.into())
    }
}

fn json_body(s: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], s).into_response()
}

pub async fn run(pool: PgPool, port: u16) -> Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/scenario/status", get(status))
        .route("/api/scenario/runs", get(runs))
        .route("/api/scenario/download/:kind", get(download))
        .route("/api/scenario/config", post(set_config))
        .with_state(pool);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "scengen web (isolated) listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn status(State(pool): State<PgPool>) -> Result<Response, AppErr> {
    let s: String = sqlx::query_scalar(
        r#"SELECT jsonb_build_object(
             'enabled',   (SELECT enabled FROM scenario.config WHERE id=1),
             'watermark', (SELECT cursor_evt FROM scenario.watermark WHERE source='move_hist'),
             'move_hist', (SELECT jsonb_build_object('rows',count(*),'min',min(comp_ts),'max',max(comp_ts))
                             FROM scenario.move_hist),
             -- `scheduled` = a systemd timer drives this stream, so a stale watermark really does
             -- mean the feed stopped. Streams without a timer (crane_deploy is collected by hand;
             -- it is the plan log, kept only for plan-vs-actual comparison) are expected to sit
             -- still and must not raise the silence alarm.
             -- KEEP IN SYNC with deploy/systemd/tt-scenario-*.timer: a stream that gains a timer but
             -- not an entry here is monitored by nobody.
             'watermarks', (SELECT coalesce(jsonb_agg(jsonb_build_object(
                        'source', source,
                        'scheduled', source IN ('move_hist','yard_move','yard_cell','gate_event'),
                        'age_s', round(EXTRACT(epoch FROM (now()-updated_at)))::int)
                        ORDER BY source), '[]'::jsonb) FROM scenario.watermark),
             'checkpoint', (SELECT jsonb_build_object(
                        'count', count(DISTINCT checkpoint_ts),
                        'age_h', round(EXTRACT(epoch FROM (now()-max(checkpoint_ts)))/3600.0, 1))
                        FROM scenario.yard_checkpoint),
             'yard_map', jsonb_build_object(
                        'blocks', (SELECT count(*) FROM scenario.yard_block),
                        'unresolved', (SELECT count(*) FROM (
                              SELECT DISTINCT m.block_id FROM scenario.yard_move m
                               WHERE m.comp_ts > now() - interval '1 day'
                                 AND NOT EXISTS (SELECT 1 FROM scenario.yard_block b
                                                  WHERE b.block_id = m.block_id)) u)),
             'enrichment', (SELECT jsonb_build_object(
                        'vessel_calls', (SELECT count(*) FROM scenario.vessel_call),
                        'containers',   (SELECT count(*) FROM scenario.container))),
             'latest_runs', (SELECT coalesce(jsonb_agg(to_jsonb(x)), '[]'::jsonb) FROM (
                        SELECT DISTINCT ON (kind) kind, run_id, state, phase, started_at, updated_at,
                               load_stats, collection
                          FROM scenario.gen_run ORDER BY kind, run_id DESC) x)
           )::text"#,
    )
    .fetch_one(&pool)
    .await?;
    Ok(json_body(s))
}

async fn runs(State(pool): State<PgPool>) -> Result<Response, AppErr> {
    let s: String = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(r) ORDER BY r.run_id DESC), '[]'::jsonb)::text FROM (
           SELECT run_id, kind, state, phase, started_at, finished_at, load_stats, collection, error_text
             FROM scenario.gen_run ORDER BY run_id DESC LIMIT 30) r",
    )
    .fetch_one(&pool)
    .await?;
    Ok(json_body(s))
}

/// Range as epoch seconds (from the UI's datetime-local inputs).
#[derive(Deserialize)]
struct Range {
    start: i64,
    end: i64,
}

/// On-demand download: assemble the requested window synchronously (LOCAL, zero Oracle) and
/// return the scenario or emulator JSON as a file attachment. No queue, no background job.
async fn download(
    State(pool): State<PgPool>,
    Path(kind): Path<String>,
    Query(r): Query<Range>,
) -> Result<Response, AppErr> {
    let ws = DateTime::from_timestamp(r.start, 0).ok_or_else(|| anyhow::anyhow!("bad start"))?;
    let we = DateTime::from_timestamp(r.end, 0).ok_or_else(|| anyhow::anyhow!("bad end"))?;
    // Bound the request. build() runs synchronously and materializes the whole scenario in memory,
    // so an unbounded window from a stray (or hostile) query string could hang or OOM this service.
    // Real scenarios are a few shifts; a week is already generous.
    if we <= ws {
        return Ok((StatusCode::BAD_REQUEST, "end must be after start").into_response());
    }
    if (we - ws).num_days() > MAX_WINDOW_DAYS {
        return Ok((
            StatusCode::BAD_REQUEST,
            format!("window too large (max {MAX_WINDOW_DAYS} days)"),
        )
            .into_response());
    }
    // Validate `kind` BEFORE assembling. build() is synchronous and materializes the whole window
    // in memory, so answering an unknown kind only AFTER that work would let a stray path burn a
    // full assembly (and one of the pool's two connections) to then return 404.
    let fname = match kind.as_str() {
        "scenario" => format!("scenario-{}.json", r.start),
        "emulator" => format!("emulator-{}.json", r.start),
        _ => return Ok((StatusCode::NOT_FOUND, "unknown kind (scenario|emulator)").into_response()),
    };
    let (mut scenario, emulator, summary) = crate::assemble::build(&pool, ws, we).await?;
    // Ship the quality/provenance summary INSIDE the file. Dropping it made a download from an
    // empty warehouse look identical to a good one — vessels all NULL-attributed into one bucket,
    // containers unenriched — with nothing in the file to say so.
    if let Some(m) = scenario.get_mut("meta").and_then(serde_json::Value::as_object_mut) {
        m.insert("summary".into(), summary);
    }
    let val = if kind == "scenario" { scenario } else { emulator };
    let body = serde_json::to_string_pretty(&val)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{fname}\"")),
        ],
        body,
    )
        .into_response())
}

#[derive(Deserialize)]
struct SetConfig {
    enabled: bool,
}

async fn set_config(
    State(pool): State<PgPool>,
    Json(b): Json<SetConfig>,
) -> Result<Response, AppErr> {
    sqlx::query("UPDATE scenario.config SET enabled=$1, updated_at=now() WHERE id=1")
        .bind(b.enabled)
        .execute(&pool)
        .await?;
    Ok(json_body(format!("{{\"enabled\":{}}}", b.enabled)))
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

const INDEX_HTML: &str = r####"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>scengen — 시나리오/에뮬 다운로드</title>
<style>
:root{color-scheme:dark}
body{font:14px/1.5 system-ui,sans-serif;margin:0;background:#0f1216;color:#d7dee6}
header{padding:14px 20px;background:#161b22;border-bottom:1px solid #2a313a;display:flex;align-items:center;gap:16px;flex-wrap:wrap}
h1{font-size:16px;margin:0;font-weight:600}
main{padding:20px;max-width:1100px;margin:0 auto;display:grid;gap:16px}
.card{background:#161b22;border:1px solid #2a313a;border-radius:10px;padding:16px}
.card h2{font-size:13px;text-transform:uppercase;letter-spacing:.04em;color:#8b98a5;margin:0 0 12px}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px}
.kv{background:#0f1216;border:1px solid #232a33;border-radius:8px;padding:8px 10px}
.kv b{display:block;font-size:11px;color:#8b98a5;font-weight:500}
.kv span{font-size:15px;font-variant-numeric:tabular-nums}
table{width:100%;border-collapse:collapse;font-size:13px}
th,td{text-align:left;padding:6px 8px;border-bottom:1px solid #232a33;white-space:nowrap}
th{color:#8b98a5;font-weight:500}
.pill{display:inline-block;padding:1px 8px;border-radius:20px;font-size:12px}
.done{background:#12351f;color:#7ee0a0}.running{background:#123049;color:#7cc4f0}
.error{background:#3a1620;color:#f09aa8}.pending{background:#2a2f18;color:#d8d38a}
button{background:#2563eb;color:#fff;border:0;border-radius:7px;padding:8px 14px;font:inherit;cursor:pointer}
button.gray{background:#2a313a;color:#d7dee6;padding:5px 10px;font-size:12px}
button.dl{background:#1f7a45}
input{background:#0f1216;border:1px solid #2a313a;color:#d7dee6;border-radius:7px;padding:7px 9px;font:inherit}
.mut{color:#8b98a5}.tag{font-size:11px;color:#8b98a5}
.avail{background:#0f1f16;border:1px solid #1f7a45;border-radius:8px;padding:10px 12px;margin-bottom:14px}
.avail b{color:#7ee0a0}
.row{display:flex;gap:8px;flex-wrap:wrap;align-items:center}
.sw{margin-left:auto;display:flex;align-items:center;gap:8px}
label.tog{position:relative;width:44px;height:24px;display:inline-block}
label.tog input{display:none}
label.tog .sl{position:absolute;inset:0;background:#3a1620;border-radius:20px;transition:.15s}
label.tog input:checked+.sl{background:#12351f}
label.tog .sl:before{content:"";position:absolute;width:18px;height:18px;left:3px;top:3px;background:#fff;border-radius:50%;transition:.15s}
label.tog input:checked+.sl:before{transform:translateX(20px)}
</style></head><body>
<header><h1>scengen · 시나리오/에뮬 다운로드</h1>
<span class="tag">isolated · non-critical</span>
<div class="sw"><span id="ksl" class="tag">수집 kill switch</span>
<label class="tog"><input type="checkbox" id="kill"><span class="sl"></span></label></div></header>
<main>
 <div class="card"><h2>다운로드</h2>
   <div class="avail" id="avail">다운로드 가능 기간을 불러오는 중…</div>
   <div class="row" style="margin-bottom:10px">
     <span class="tag">프리셋:</span>
     <button class="gray" onclick="preset('cov')">가능 기간 전체</button>
     <button class="gray" onclick="preset(1)">최근 1시간</button>
     <button class="gray" onclick="preset(8)">최근 8시간</button>
     <button class="gray" onclick="preset(24)">최근 24시간</button>
   </div>
   <div class="row">
     <label class="tag">시작 <input type="datetime-local" id="ws" step="1"></label>
     <label class="tag">끝 <input type="datetime-local" id="we" step="1"></label>
     <button class="dl" onclick="dl('scenario')">시나리오 다운로드</button>
     <button class="dl" onclick="dl('emulator')">에뮬 스펙 다운로드</button>
   </div>
   <div class="tag" id="msg" style="margin-top:8px">기간을 고르면 그 기간의 데이터로 즉석 조립해 JSON을 내려받습니다(자동 생성 없음 · TOS 미접촉).</div>
 </div>
 <div class="card"><h2>수집 현황</h2><div class="grid" id="stat"></div></div>
 <div class="card"><h2>수집기 최근 실행</h2><div id="runs"></div></div>
</main>
<script>
const $=s=>document.querySelector(s), H=x=>x==null?'<span class=mut>–</span>':x;
const pill=s=>`<span class="pill ${s}">${s}</span>`;
let COV={min:null,max:null};
async function j(u){const r=await fetch(u);return r.json()}
function fmt(t){return t?new Date(t).toLocaleString():'–'}
function loc(d){const p=n=>String(n).padStart(2,'0');return `${d.getFullYear()}-${p(d.getMonth()+1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`}
function preset(k){if(!COV.min){msg('아직 수집된 데이터가 없습니다');return}
  let s,e;
  if(k==='cov'){s=new Date(COV.min);e=new Date(COV.max)}
  else{e=new Date(COV.max);s=new Date(Math.max(new Date(COV.min),new Date(COV.max)-k*3600e3))}
  $('#ws').value=loc(s);$('#we').value=loc(e)}
function msg(t){$('#msg').textContent=t}
function dl(kind){
  const ws=$('#ws').value,we=$('#we').value;
  if(!ws||!we){msg('기간을 설정하세요');return}
  const s=Math.floor(new Date(ws).getTime()/1000),e=Math.floor(new Date(we).getTime()/1000);
  if(e<=s){msg('끝이 시작보다 뒤여야 합니다');return}
  msg(kind+' 조립·다운로드 중…');
  window.location='/api/scenario/download/'+kind+'?start='+s+'&end='+e;
  setTimeout(()=>msg('다운로드가 시작되지 않으면 기간에 데이터가 없는지 확인하세요.'),1500);
}
let first=true;
async function load(){
  const s=await j('/api/scenario/status');
  $('#kill').checked=!!s.enabled;
  $('#ksl').textContent=s.enabled?'수집 ON':'수집 OFF';
  const mh=s.move_hist||{},ym=s.yard_map||{},en=s.enrichment||{};
  COV={min:mh.min,max:mh.max};
  $('#avail').innerHTML=mh.min
    ?`다운로드 가능 기간: <b>${fmt(mh.min)}</b> ~ <b>${fmt(mh.max)}</b> · 이동 ${mh.rows}건 · enrich 선박 ${H(en.vessel_calls)}·컨 ${H(en.containers)}`
    :'아직 수집된 이동 데이터가 없습니다. (수집기 collect가 돌면 여기 범위가 표시됩니다)';
  if(first&&mh.min){preset('cov');first=false}
  $('#stat').innerHTML=[
    ['이동 데이터',H(mh.rows)+'건'],['watermark',H(s.watermark)],
    ['야드 블록맵',H(ym.blocks)+'개'+(ym.unresolved>0?' <b style="color:#f87171">⚠미해석 '+ym.unresolved+'</b>':'')],
    ['enrich 선박·컨',H(en.vessel_calls)+' · '+H(en.containers)],
    ['수집기 침묵',silence(s.watermarks||[])],
    ['야드 체크포인트',ckpt(s.checkpoint||{})],
  ].map(([k,v])=>`<div class=kv><b>${k}</b><span>${v}</span></div>`).join('');
  $('#runs').innerHTML=table(s.latest_runs||[],['kind','state','건강','load','수집','시각'],r=>[
    r.kind,pill(r.state),health(r),cell(r.load_stats),cell(r.collection),fmt(r.updated_at)]);
}
// Checkpoints are what keep a download's yard replay bounded. If they stop being written the
// downloads still return correct data, just progressively slower — so surface staleness here.
function ckpt(c){
  if(!c.count)return'<b style="color:#f87171">⚠ 없음 (다운로드가 전체 재생)</b>';
  return c.age_h>12?`<b style="color:#f87171">⚠ ${c.count}개 · ${c.age_h}h 정체</b>`:`${c.count}개 · ${c.age_h}h 전`;
}
// Data silence: the worst watermark age AMONG TIMER-DRIVEN streams. A watermark only advances when
// new rows arrive, so a long age means the feed stopped even though the timer may still be firing
// (fetched=0 every tick). Hand-run streams are excluded — they are always stale by design, and
// including them pinned this tile red permanently, which trains everyone to ignore it.
function silence(wms){
  const s=(wms||[]).filter(w=>w.scheduled);
  if(!s.length)return'<span class=mut>–</span>';
  const w=s.reduce((a,b)=>(b.age_s||0)>(a.age_s||0)?b:a);
  const m=Math.round((w.age_s||0)/60);
  return m>30?`<b style="color:#f87171">⚠ ${w.source} ${m}분 무진행</b>`:`최대 ${m}분 (${w.source})`;
}
// Early warning that the PK/INDEX seek stopped working: there is no Oracle-side statement timeout,
// so a plan flip to a full scan just shows up as a slow poll repeating quietly. Baseline toolbox
// round-trip is ~0.8s, so >5s means the query itself is doing real work again.
function health(r){
  const ms=+((r.load_stats||{}).query_ms||0), w=[];
  const age=(Date.now()-new Date(r.updated_at))/1000;
  if(ms>5000)w.push('⚠느림 '+ms+'ms');
  if(age>3600&&!['snapshot','assemble'].includes(r.kind))w.push('⚠정지 '+Math.round(age/60)+'분');
  return w.length?`<b style="color:#f87171">${w.join(' · ')}</b>`:'<span class=mut>OK</span>';
}
function cell(o){if(!o||!Object.keys(o).length)return'<span class=mut>–</span>';return'<span class=tag>'+Object.entries(o).filter(([k])=>!k.startsWith('_')).map(([k,v])=>`${k}:${typeof v=='object'?JSON.stringify(v):v}`).join(' · ')+'</span>'}
function table(rows,cols,fn){if(!rows.length)return'<span class=mut>없음</span>';
  return'<table><tr>'+cols.map(c=>`<th>${c}</th>`).join('')+'</tr>'+
    rows.map(r=>'<tr>'+fn(r).map(c=>`<td>${c}</td>`).join('')+'</tr>').join('')+'</table>'}
$('#kill').onchange=async e=>{await fetch('/api/scenario/config',{method:'POST',
  headers:{'content-type':'application/json'},body:JSON.stringify({enabled:e.target.checked})});load()};
load();setInterval(load,5000);
</script></body></html>"####;
