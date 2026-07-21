//! Isolated read/monitor web service for the scenario subsystem. A SEPARATE axum server
//! (own process/port) — never mounted on the critical dashboard API — so a fault here can't
//! affect the dashboard. Reads scenario.* for monitoring; writes only intents (enqueue an
//! assembly_job, flip the kill switch). Assembly/collection happen in the other subcommands.
//!
//! JSON is built in Postgres and returned as text (cast ::text), so we don't need the sqlx
//! `json` feature (which would touch the shared workspace).

use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::PgPool;

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
        .route("/api/scenario/jobs", get(list_jobs).post(create_job))
        .route("/api/scenario/jobs/:id", get(get_job))
        .route("/api/scenario/jobs/:id/scenario", get(job_scenario))
        .route("/api/scenario/jobs/:id/emulator", get(job_emulator))
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
             'yard_snapshots', (SELECT jsonb_build_object('count',count(DISTINCT snapshot_ts),'latest',max(snapshot_ts))
                             FROM scenario.yard_snapshot),
             'enrichment', (SELECT jsonb_build_object(
                        'vessel_calls', (SELECT count(*) FROM scenario.vessel_call),
                        'containers',   (SELECT count(*) FROM scenario.container))),
             'jobs', (SELECT jsonb_build_object(
                        'pending',count(*) FILTER (WHERE state='pending'),
                        'done',   count(*) FILTER (WHERE state='done'),
                        'error',  count(*) FILTER (WHERE state='error'))
                        FROM scenario.assembly_job),
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

async fn list_jobs(State(pool): State<PgPool>) -> Result<Response, AppErr> {
    let s: String = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(j) ORDER BY j.job_id DESC), '[]'::jsonb)::text FROM (
           SELECT job_id, window_start, window_end, state, summary, requested_at, finished_at, error_text
             FROM scenario.assembly_job ORDER BY job_id DESC LIMIT 50) j",
    )
    .fetch_one(&pool)
    .await?;
    Ok(json_body(s))
}

#[derive(Deserialize)]
struct CreateJob {
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
}

async fn create_job(
    State(pool): State<PgPool>,
    Json(b): Json<CreateJob>,
) -> Result<Response, AppErr> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO scenario.assembly_job (window_start, window_end) VALUES ($1,$2) RETURNING job_id",
    )
    .bind(b.window_start)
    .bind(b.window_end)
    .fetch_one(&pool)
    .await?;
    Ok(json_body(format!("{{\"job_id\":{id}}}")))
}

async fn get_job(State(pool): State<PgPool>, Path(id): Path<i64>) -> Result<Response, AppErr> {
    let s: Option<String> = sqlx::query_scalar(
        "SELECT to_jsonb(j)::text FROM scenario.assembly_job j WHERE job_id=$1",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await?;
    Ok(match s {
        Some(s) => json_body(s),
        None => (StatusCode::NOT_FOUND, "no such job").into_response(),
    })
}

async fn job_scenario(State(pool): State<PgPool>, Path(id): Path<i64>) -> Result<Response, AppErr> {
    job_col(&pool, id, "scenario_out").await
}
async fn job_emulator(State(pool): State<PgPool>, Path(id): Path<i64>) -> Result<Response, AppErr> {
    job_col(&pool, id, "emulator_out").await
}
async fn job_col(pool: &PgPool, id: i64, col: &str) -> Result<Response, AppErr> {
    let sql = format!("SELECT coalesce({col}::text, 'null') FROM scenario.assembly_job WHERE job_id=$1");
    let s: Option<String> = sqlx::query_scalar(&sql).bind(id).fetch_optional(pool).await?;
    Ok(match s {
        Some(s) => json_body(s),
        None => (StatusCode::NOT_FOUND, "no such job").into_response(),
    })
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
<title>scengen — 시나리오/에뮬 수집 관리</title>
<style>
:root{color-scheme:dark}
body{font:14px/1.5 system-ui,sans-serif;margin:0;background:#0f1216;color:#d7dee6}
header{padding:14px 20px;background:#161b22;border-bottom:1px solid #2a313a;display:flex;align-items:center;gap:16px;flex-wrap:wrap}
h1{font-size:16px;margin:0;font-weight:600}
main{padding:20px;max-width:1150px;margin:0 auto;display:grid;gap:16px}
.card{background:#161b22;border:1px solid #2a313a;border-radius:10px;padding:14px 16px}
.card h2{font-size:13px;text-transform:uppercase;letter-spacing:.04em;color:#8b98a5;margin:0 0 10px}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px}
.kv{background:#0f1216;border:1px solid #232a33;border-radius:8px;padding:8px 10px}
.kv b{display:block;font-size:11px;color:#8b98a5;font-weight:500}
.kv span{font-size:15px;font-variant-numeric:tabular-nums}
table{width:100%;border-collapse:collapse;font-size:13px}
th,td{text-align:left;padding:6px 8px;border-bottom:1px solid #232a33;white-space:nowrap;vertical-align:top}
th{color:#8b98a5;font-weight:500}
.pill{display:inline-block;padding:1px 8px;border-radius:20px;font-size:12px}
.done{background:#12351f;color:#7ee0a0}.running{background:#123049;color:#7cc4f0}
.error{background:#3a1620;color:#f09aa8}.pending{background:#2a2f18;color:#d8d38a}
button{background:#2563eb;color:#fff;border:0;border-radius:7px;padding:7px 12px;font:inherit;cursor:pointer}
button.gray{background:#2a313a;color:#d7dee6;padding:5px 10px;font-size:12px}
a.dl{display:inline-block;background:#1f3b2a;color:#7ee0a0;text-decoration:none;border-radius:6px;padding:3px 9px;font-size:12px;margin-right:5px}
input{background:#0f1216;border:1px solid #2a313a;color:#d7dee6;border-radius:7px;padding:6px 8px;font:inherit}
.mut{color:#8b98a5}.tag{font-size:11px;color:#8b98a5}
.sw{margin-left:auto;display:flex;align-items:center;gap:8px}
label.tog{position:relative;width:44px;height:24px;display:inline-block}
label.tog input{display:none}
label.tog .sl{position:absolute;inset:0;background:#3a1620;border-radius:20px;transition:.15s}
label.tog input:checked+.sl{background:#12351f}
label.tog .sl:before{content:"";position:absolute;width:18px;height:18px;left:3px;top:3px;background:#fff;border-radius:50%;transition:.15s}
label.tog input:checked+.sl:before{transform:translateX(20px)}
.row{display:flex;gap:8px;flex-wrap:wrap;align-items:center}
</style></head><body>
<header><h1>scengen · 시나리오/에뮬 수집 관리</h1>
<span class="tag">isolated · non-critical</span>
<div class="sw"><span id="ksl" class="tag">kill switch</span>
<label class="tog"><input type="checkbox" id="kill"><span class="sl"></span></label></div></header>
<main>
 <div class="card"><h2>수집 현황</h2><div class="grid" id="stat"></div>
   <div class="tag" id="cov" style="margin-top:8px"></div></div>
 <div class="card"><h2>기간 설정 → 시나리오/에뮬 생성 · 다운로드</h2>
   <div class="row" style="margin-bottom:8px">
     <span class="tag">프리셋:</span>
     <button class="gray" onclick="preset('cov')">수집범위 전체</button>
     <button class="gray" onclick="preset(1)">최근 1시간</button>
     <button class="gray" onclick="preset(8)">최근 8시간(시프트)</button>
     <button class="gray" onclick="preset(24)">최근 24시간</button>
   </div>
   <div class="row">
     <label class="tag">start <input type="datetime-local" id="ws" step="1"></label>
     <label class="tag">end <input type="datetime-local" id="we" step="1"></label>
     <button onclick="enqueue()">생성 요청</button>
     <span class="mut" id="emsg"></span></div>
   <div class="tag" style="margin-top:6px">요청하면 scengen assemble가 로컬에서 조립합니다(TOS 미접촉). 완료되면 아래에서 다운로드.</div></div>
 <div class="card"><h2>생성된 시나리오 (다운로드)</h2><div id="jobs"></div></div>
 <div class="card"><h2>최근 실행 (kind별)</h2><div id="runs"></div></div>
</main>
<script>
const $=s=>document.querySelector(s), H=x=>x==null?'<span class=mut>–</span>':x;
const pill=s=>`<span class="pill ${s}">${s}</span>`;
let COV={min:null,max:null};
async function j(u,o){const r=await fetch(u,o);return r.json()}
function fmt(t){return t?new Date(t).toLocaleString():'–'}
function loc(d){const p=n=>String(n).padStart(2,'0');return `${d.getFullYear()}-${p(d.getMonth()+1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`}
function preset(k){let s,e=new Date();
  if(k==='cov'){if(!COV.min)return;s=new Date(COV.min);e=new Date(COV.max)}
  else{s=new Date(Date.now()-k*3600e3)}
  $('#ws').value=loc(s);$('#we').value=loc(e)}
async function load(){
  const s=await j('/api/scenario/status');
  $('#kill').checked=!!s.enabled;
  $('#ksl').textContent=s.enabled?'ON (수집중)':'OFF (정지)';
  const mh=s.move_hist||{},ys=s.yard_snapshots||{},jb=s.jobs||{},en=s.enrichment||{};
  COV={min:mh.min,max:mh.max};
  $('#stat').innerHTML=[
    ['move_hist rows',H(mh.rows)],['watermark',H(s.watermark)],
    ['yard 스냅샷',H(ys.count)+' · '+fmt(ys.latest)],
    ['enrich 선박·컨',H(en.vessel_calls)+' · '+H(en.containers)],
    ['잡 pending/done/err',`${jb.pending||0}/${jb.done||0}/${jb.error||0}`],
  ].map(([k,v])=>`<div class=kv><b>${k}</b><span>${v}</span></div>`).join('');
  $('#cov').textContent=mh.min?`수집된 이동 데이터 범위: ${fmt(mh.min)} ~ ${fmt(mh.max)}`:'아직 수집된 이동 데이터가 없습니다.';
  $('#runs').innerHTML=table(s.latest_runs||[],['kind','state','phase','load_stats','collection','updated_at'],r=>[
    r.kind,pill(r.state),H(r.phase),cell(r.load_stats),cell(r.collection),fmt(r.updated_at)]);
  const jobs=await j('/api/scenario/jobs');
  $('#jobs').innerHTML=table(jobs,['job','기간','state','내용','다운로드'],r=>[
    r.job_id,`${fmt(r.window_start)}<br><span class=mut>~ ${fmt(r.window_end)}</span>`,pill(r.state),
    r.state=='done'?sumcell(r.summary):(r.error_text?`<span class=error>${r.error_text.slice(0,50)}</span>`:'<span class=mut>…</span>'),
    r.state=='done'?`<a class=dl href="/api/scenario/jobs/${r.job_id}/scenario" download="scenario-${r.job_id}.json">시나리오</a><a class=dl href="/api/scenario/jobs/${r.job_id}/emulator" download="emulator-${r.job_id}.json">에뮬</a>`:'–']);
}
function sumcell(o){if(!o)return'<span class=mut>–</span>';
  return `<span class=tag>선박 ${o.vessels} · 컨 ${o.containers} (DS ${o.ds}/LD ${o.ld}) · enrich ${o.enriched_pct}% · qc표본 ${o.qc_sample}·yc ${o.yc_sample}</span>`}
function cell(o){if(!o||!Object.keys(o).length)return'<span class=mut>–</span>';return'<span class=tag>'+Object.entries(o).filter(([k])=>!k.startsWith('_')).map(([k,v])=>`${k}:${typeof v=='object'?JSON.stringify(v):v}`).join(' · ')+'</span>'}
function table(rows,cols,fn){if(!rows.length)return'<span class=mut>없음</span>';
  return'<table><tr>'+cols.map(c=>`<th>${c}</th>`).join('')+'</tr>'+
    rows.map(r=>'<tr>'+fn(r).map(c=>`<td>${c}</td>`).join('')+'</tr>').join('')+'</table>'}
async function enqueue(){
  const ws=$('#ws').value,we=$('#we').value;
  if(!ws||!we){$('#emsg').textContent='기간을 입력하세요';return}
  const r=await j('/api/scenario/jobs',{method:'POST',headers:{'content-type':'application/json'},
    body:JSON.stringify({window_start:new Date(ws).toISOString(),window_end:new Date(we).toISOString()})});
  $('#emsg').textContent='잡 #'+r.job_id+' 요청됨';load();
}
$('#kill').onchange=async e=>{await j('/api/scenario/config',{method:'POST',
  headers:{'content-type':'application/json'},body:JSON.stringify({enabled:e.target.checked})});load()};
load();setInterval(load,5000);
</script></body></html>"####;
