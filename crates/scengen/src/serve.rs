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
        // Async path. `download` assembles inline and holds one of the pool's two connections for
        // the whole build — measured 1.5s for 8 hours but 22s for a day, and the cap is a week. A
        // long window on the sync path is therefore a request that may outlive the client's patience
        // (or its proxy) while occupying half this service's database capacity. Queue it instead:
        // the worker builds it out of process, under its own memory limit, and the result waits.
        .route("/api/scenario/jobs", get(jobs).post(enqueue))
        .route("/api/scenario/jobs/:job_id", get(job))
        .route("/api/scenario/jobs/:job_id/download/:kind", get(job_download))
        .route("/api/scenario/config", post(set_config))
        .with_state(pool);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "scengen web (isolated) listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn status(State(pool): State<PgPool>) -> Result<Response, AppErr> {
    // What can actually be assembled — NOT what there are rows for. The page used to derive its
    // "available period" from move_hist and so offered weeks that would have produced a scenario
    // with no crane work order at all.
    let usable = crate::assemble::usable_range(&pool).await?;
    let s: String = sqlx::query_scalar(
        r#"SELECT jsonb_build_object(
             'enabled',   (SELECT enabled FROM scenario.config WHERE id=1),
             'watermark', (SELECT cursor_evt FROM scenario.watermark WHERE source='move_hist'),
             'move_hist', (SELECT jsonb_build_object('rows',count(*),'min',min(comp_ts),'max',max(comp_ts))
                             FROM scenario.move_hist),
             -- `scheduled` = a systemd timer drives this stream, so a stale watermark really does
             -- mean the feed stopped. A stream without a timer is expected to sit still and must
             -- not raise the silence alarm — but it is also, by now, a sign that the stream was
             -- retired and its cursor row outlived it (that is how the dead crane_deploy collector
             -- was found: an unscheduled row that had not moved in eleven days).
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
    let mut v: serde_json::Value = serde_json::from_str(&s)?;
    if let Some(o) = v.as_object_mut() {
        o.insert("usable_range".into(), usable);
    }
    Ok(json_body(v.to_string()))
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
    // Bound the request. build() runs synchronously and materializes the whole scenario in memory,
    // so an unbounded window from a stray (or hostile) query string could hang or OOM this service.
    // Real scenarios are a few shifts; a week is already generous. Past a few hours prefer the
    // queue (POST /api/scenario/jobs), which builds out of process under its own memory cap.
    let (ws, we) = match check_window(&pool, r.start, r.end).await {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
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

/// Validate a requested window once, for both the sync and the async path.
///
/// The range check is the point of this function, not a formality. Every window served from here is
/// a window a scenario is COHERENT over — before, the endpoint would happily assemble any period it
/// had rows for and hand back a file whose crane work list was silently empty, with only a
/// percentage buried in `meta.summary` to say so. A refusal that names the reason is a better
/// answer than a plausible file that is wrong.
async fn check_window(
    pool: &PgPool,
    start: i64,
    end: i64,
) -> Result<(DateTime<chrono::Utc>, DateTime<chrono::Utc>), Response> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, m).into_response();
    let ws = DateTime::from_timestamp(start, 0).ok_or_else(|| bad("bad start".into()))?;
    let we = DateTime::from_timestamp(end, 0).ok_or_else(|| bad("bad end".into()))?;
    if we <= ws {
        return Err(bad("end must be after start".into()));
    }
    if (we - ws).num_days() > MAX_WINDOW_DAYS {
        return Err(bad(format!("window too large (max {MAX_WINDOW_DAYS} days)")));
    }
    let r = crate::assemble::usable_range(pool)
        .await
        .map_err(|e| AppErr(e).into_response())?;
    let ts = |k: &str| r.get(k).and_then(|v| v.as_str()).and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&chrono::Utc));
    let (Some(lo), Some(hi)) = (ts("from"), ts("to")) else {
        return Err(bad("no scenario data collected yet".into()));
    };
    let why = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("?").to_string();
    let span = format!("{} ~ {}", lo.to_rfc3339(), hi.to_rfc3339());
    if ws < lo {
        return Err(bad(format!(
            "window starts before the assemblable period ({span}). \
             the early end is set by {} — a scenario before it has no crane work order and would replay an empty quay.",
            why("from_reason")
        )));
    }
    if we > hi {
        return Err(bad(format!(
            "window ends after the assemblable period ({span}). \
             the late end is where {} has collected up to; that period is real but not yet complete.",
            why("to_reason")
        )));
    }
    Ok((ws, we))
}

/// Queue a window for background assembly. Returns the job id immediately.
///
/// Deliberately idempotent on (window, state): asking twice for the same period while the first
/// request is still pending or running returns the SAME job rather than building it twice. A 22 MB
/// assembly is expensive enough that an impatient double-click should not cost two of them, and
/// two workers racing on one window would produce two identical rows for no gain.
async fn enqueue(State(pool): State<PgPool>, Query(r): Query<Range>) -> Result<Response, AppErr> {
    let (ws, we) = match check_window(&pool, r.start, r.end).await {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let existing: Option<(i64, String)> = sqlx::query_as(
        "SELECT job_id, state FROM scenario.assembly_job
          WHERE window_start = $1 AND window_end = $2 AND state IN ('pending','running')
          ORDER BY requested_at LIMIT 1",
    )
    .bind(ws).bind(we)
    .fetch_optional(&pool)
    .await?;
    if let Some((job_id, state)) = existing {
        return Ok(Json(serde_json::json!({
            "job_id": job_id, "state": state, "note": "already queued for this window"
        }))
        .into_response());
    }
    let (job_id,): (i64,) = sqlx::query_as(
        "INSERT INTO scenario.assembly_job (window_start, window_end) VALUES ($1, $2) RETURNING job_id",
    )
    .bind(ws).bind(we)
    .fetch_one(&pool)
    .await?;
    tracing::info!(job_id, %ws, %we, "assembly job queued");
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job_id": job_id, "state": "pending" })),
    )
        .into_response())
}

/// Recent jobs, newest first. The output columns are deliberately NOT selected — each is tens of
/// megabytes, and a list endpoint that drags them along would be unusable at exactly the moment the
/// queue is busiest.
async fn jobs(State(pool): State<PgPool>) -> Result<Response, AppErr> {
    let s: String = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(jsonb_build_object(
                  'job_id', job_id, 'state', state,
                  'window', jsonb_build_array(window_start, window_end),
                  'requested_at', requested_at, 'finished_at', finished_at,
                  'error', error_text, 'summary', summary,
                  'bytes', coalesce(pg_column_size(scenario_out), 0)
                            + coalesce(pg_column_size(emulator_out), 0))
                ORDER BY job_id DESC), '[]'::jsonb)::text
           FROM (SELECT * FROM scenario.assembly_job ORDER BY job_id DESC LIMIT 50) t",
    )
    .fetch_one(&pool)
    .await?;
    Ok(json_body(s))
}

/// One job's state. Poll this after enqueue; `state` goes pending -> running -> done | error.
async fn job(State(pool): State<PgPool>, Path(job_id): Path<i64>) -> Result<Response, AppErr> {
    let s: Option<String> = sqlx::query_scalar(
        "SELECT jsonb_build_object(
                  'job_id', job_id, 'state', state,
                  'window', jsonb_build_array(window_start, window_end),
                  'requested_at', requested_at, 'finished_at', finished_at,
                  'error', error_text, 'summary', summary,
                  'bytes', coalesce(pg_column_size(scenario_out), 0)
                            + coalesce(pg_column_size(emulator_out), 0))::text
           FROM scenario.assembly_job WHERE job_id = $1",
    )
    .bind(job_id)
    .fetch_optional(&pool)
    .await?;
    match s {
        Some(s) => Ok(json_body(s)),
        None => Ok((StatusCode::NOT_FOUND, "no such job").into_response()),
    }
}

/// Fetch a finished job's output. Streams the stored jsonb straight out as text — the process never
/// parses it back into a Value, so serving a large result costs a copy rather than a second build.
async fn job_download(
    State(pool): State<PgPool>,
    Path((job_id, kind)): Path<(i64, String)>,
) -> Result<Response, AppErr> {
    let col = match kind.as_str() {
        "scenario" => "scenario_out",
        "emulator" => "emulator_out",
        _ => return Ok((StatusCode::NOT_FOUND, "unknown kind (scenario|emulator)").into_response()),
    };
    // state is fetched with the body so a job that is still running gets told so, instead of
    // receiving an empty 404 that reads like the job never existed.
    let row: Option<(String, Option<String>, DateTime<chrono::Utc>)> = sqlx::query_as(&format!(
        "SELECT state, {col}::text, window_start FROM scenario.assembly_job WHERE job_id = $1"
    ))
    .bind(job_id)
    .fetch_optional(&pool)
    .await?;
    let Some((state, body, ws)) = row else {
        return Ok((StatusCode::NOT_FOUND, "no such job").into_response());
    };
    let Some(body) = body else {
        return Ok((
            StatusCode::CONFLICT,
            format!("job is '{state}', not done — poll /api/scenario/jobs/{job_id}"),
        )
            .into_response());
    };
    let fname = format!("{kind}-{}.json", ws.timestamp());
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
     <button class="gray" onclick="preset(8)">최근 8시간</button>
     <button class="gray" onclick="preset(24)">최근 24시간</button>
     <button class="gray" onclick="preset(72)">최근 3일</button>
   </div>
   <div class="row">
     <label class="tag">시작 <input type="datetime-local" id="ws" step="1"></label>
     <label class="tag">끝 <input type="datetime-local" id="we" step="1"></label>
     <button class="dl" onclick="dl('scenario')">시나리오 다운로드</button>
     <button class="dl" onclick="dl('emulator')">에뮬 스펙 다운로드</button>
   </div>
   <div class="tag" id="msg" style="margin-top:8px">기간을 고르면 그 기간의 데이터로 즉석 조립해 JSON을 내려받습니다(자동 생성 없음 · TOS 미접촉).</div>
   <div class="row" style="margin-top:10px">
     <span class="tag">긴 기간은 큐로:</span>
     <button class="gray" onclick="q()">작업큐에 넣기</button>
     <span class="tag" id="qmsg"></span>
   </div>
   <div id="jobs" style="margin-top:10px"></div>
 </div>
 <div class="card"><h2>수집 현황</h2><div class="grid" id="stat"></div></div>
 <div class="card"><h2>수집기 최근 실행</h2><div id="runs"></div></div>
</main>
<script>
const $=s=>document.querySelector(s), H=x=>x==null?'<span class=mut>–</span>':x;
const pill=s=>`<span class="pill ${s}">${s}</span>`;
let COV={min:null,max:null};
// Mirrors MAX_WINDOW_DAYS in serve.rs — the largest single window the server will assemble.
const MAXD=7;
async function j(u){const r=await fetch(u);return r.json()}
function fmt(t){return t?new Date(t).toLocaleString():'–'}
function loc(d){const p=n=>String(n).padStart(2,'0');return `${d.getFullYear()}-${p(d.getMonth()+1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`}
function preset(k){if(!COV.min){msg('아직 조립 가능한 기간이 없습니다');return}
  const lo=new Date(COV.min),hi=new Date(COV.max);
  // The assemblable period grows by a day every day, so "전체" will eventually exceed the
  // single-window cap. Clamp to the newest MAXD days rather than let the button start producing
  // a request the server has to refuse.
  let e=hi, s=(k==='cov')?lo:new Date(Math.max(lo,hi-k*3600e3));
  if(hi-s>MAXD*86400e3){s=new Date(hi-MAXD*86400e3);
    if(k==='cov')msg(`조립 가능 기간이 ${MAXD}일을 넘어 최근 ${MAXD}일로 맞췄습니다 — 더 이전은 시작 시각을 직접 지정하세요.`)}
  $('#ws').value=loc(s);$('#we').value=loc(e)}
function msg(t){$('#msg').textContent=t}
// Client-side guard mirroring the server's. The server refuses out-of-range windows anyway; this
// exists so the answer is instant and phrased the same way, not so the check lives in one place.
function pick(){
  const ws=$('#ws').value,we=$('#we').value;
  if(!ws||!we){msg('기간을 설정하세요');return null}
  const s=Math.floor(new Date(ws).getTime()/1000),e=Math.floor(new Date(we).getTime()/1000);
  if(e<=s){msg('끝이 시작보다 뒤여야 합니다');return null}
  if(COV.min&&s<Math.floor(new Date(COV.min).getTime()/1000)){
    msg('조립 가능 기간보다 이릅니다 — 그 이전은 크레인 작업 순서가 없어 빈 안벽이 재생됩니다.');return null}
  if(COV.max&&e>Math.floor(new Date(COV.max).getTime()/1000)){
    msg('조립 가능 기간보다 늦습니다 — 가장 느린 수집기가 아직 거기까지 못 갔습니다.');return null}
  return [s,e];
}
function dl(kind){
  const p=pick(); if(!p)return;
  msg(kind+' 조립·다운로드 중…');
  window.location='/api/scenario/download/'+kind+'?start='+p[0]+'&end='+p[1];
  setTimeout(()=>msg('오래 걸리면 아래 작업큐를 쓰세요 — 하루 창이 약 23초입니다.'),1500);
}
async function q(){
  const p=pick(); if(!p)return;
  const r=await fetch('/api/scenario/jobs?start='+p[0]+'&end='+p[1],{method:'POST'});
  const b=await r.json().catch(()=>null);
  $('#qmsg').textContent=r.ok?`job ${b.job_id} — ${b.note||b.state}`:(b&&b.error)||await r.text().catch(()=>'실패');
  loadJobs();
}
async function loadJobs(){
  const js=await j('/api/scenario/jobs');
  $('#jobs').innerHTML=js.length?table(js.slice(0,8),['job','상태','기간','크기','받기'],r=>[
    r.job_id,pill(r.state),fmt(r.window[0])+' ~ '+fmt(r.window[1]),
    r.bytes?(r.bytes/1048576).toFixed(1)+'MB':'<span class=mut>–</span>',
    r.state==='done'
      ?`<a href="/api/scenario/jobs/${r.job_id}/download/scenario">시나리오</a> · <a href="/api/scenario/jobs/${r.job_id}/download/emulator">에뮬</a>`
      :(r.error?`<span class=mut title="${(r.error+'').replace(/"/g,'&quot;')}">실패</span>`:'<span class=mut>대기</span>')]):'';
}
let first=true;
async function load(){
  const s=await j('/api/scenario/status');
  $('#kill').checked=!!s.enabled;
  $('#ksl').textContent=s.enabled?'수집 ON':'수집 OFF';
  const mh=s.move_hist||{},ym=s.yard_map||{},en=s.enrichment||{},ur=s.usable_range||{};
  // The range offered is the range a scenario is COHERENT over, not the range we hold rows for.
  // Those differ by weeks: the landside streams reach back to June, but a replay needs the crane
  // work order, which starts when TOS began writing bay labels.
  COV={min:ur.from,max:ur.to};
  $('#ws').min=$('#we').min=ur.from?loc(new Date(ur.from)):'';
  $('#ws').max=$('#we').max=ur.to?loc(new Date(ur.to)):'';
  const days=ur.from&&ur.to?((new Date(ur.to)-new Date(ur.from))/86400e3).toFixed(1):null;
  $('#avail').innerHTML=ur.from
    ?`조립 가능 기간: <b>${fmt(ur.from)}</b> ~ <b>${fmt(ur.to)}</b> <span class=tag>(${days}일)</span><br>
      <span class=tag>시작은 <b>${H(ur.from_reason)}</b>가 정합니다 — 그 이전에는 크레인 작업 순서가 없어 안벽이 빈 채로 재생됩니다.
      끝은 <b>${H(ur.to_reason)}</b>가 수집한 지점입니다 — 그 뒤 데이터는 실재하지만 아직 완결이 아닙니다.
      이 범위 밖은 다운로드가 거부됩니다. 보유 이동 데이터 자체는 ${fmt(mh.min)}부터 ${H(mh.rows)}건.</span>`
    :'아직 조립 가능한 기간이 없습니다. (수집기가 돌면 여기 범위가 표시됩니다)';
  if(first&&ur.from){preset('cov');first=false;loadJobs()}
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
