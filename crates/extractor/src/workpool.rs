//! Live work-pool snapshot extract. ONE bounded Oracle round-trip per 60s tick
//! (CHUNK8 2026-08-10: down from two — the JOB_QUEUE_SCHEDULE scan rides the same
//! UNION ALL as the JOB_ORDER_LIST scan, discriminated by SRC; round-trip fixed cost
//! ~2s dominates payload, so fewer round-trips is the lever, not fewer rows.
//! History: CHUNK7 7-1(a) folded SQL_ASSIGNED in, 7-1(b) split vessel_schedule out):
//!   SRC='WQ' JOB_QUEUE_SCHEDULE → live_workqueue (per-QC queue plan + progress).
//!   SRC='WP' JOB_ORDER_LIST (A + B + Q, any jobtype) → split in Rust into:
//!        - live_assigned_tt (any row with a non-empty YTNO — the old SQL_ASSIGNED
//!                            population: any jobtype, status A/B/Q)
//!        - live_workpool  (DS/LD + A = dispatched in-flight moves, the QC task cards)
//!        - live_candidate (DS/LD + Q + empty YTNO = UNASSIGNED demand, aggregated:
//!                          discharge by QC, load by source block — dispatch candidate pool)
//! Kill switch: env WORKPOOL_FETCH=split reverts to the two separate scans.
//! This is the ONLY path that brings the work pool into Postgres; the API crate can't
//! reach Oracle. "Live now" (no date window) — bounded by status + recent CRE_DT to
//! keep the scan small and Oracle-friendly.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::runner::Toolbox;

const SQL_WORKQUEUE: &str = include_str!("../sql/workqueue.sql");
const SQL_WORKPOOL: &str = include_str!("../sql/workpool.sql");
const SQL_POOL_TICK: &str = include_str!("../sql/pool_tick.sql");
const SQL_VESSEL_SCHEDULE: &str = include_str!("../sql/vessel_schedule.sql");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct VesselScheduleRow {
    pub vessel: String,
    pub voyage: String,
    pub status: Option<String>,
    pub berthno: Option<String>,
    pub estber: Option<String>,
    pub estwkc: Option<String>,
    pub estdep: Option<String>,
    pub cutoff: Option<String>,
    pub actber: Option<String>,
    pub actdep: Option<String>,
    pub disvan: Option<f64>,
    pub loadvan: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct QueueRow {
    pub qc: String,
    pub vessel: String,
    pub voyage: Option<String>,
    pub queuename: String,
    pub disload: Option<String>,
    pub seq: Option<i64>,
    pub total_qty: Option<i64>,
    pub comp_qty: Option<i64>,
    pub plan_qty: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct MoveRow {
    // Option: the widened WHERE (CHUNK7 7-1(a), any jobtype) also returns yard-only
    // jobtypes (GO/RH/GI/MO/MI/AH/GC) that have no QUEUENAME (no vessel queue). DS/LD
    // rows always carry one (verified: 0/683 null in a live probe) — the only rows that
    // ever reach live_workpool/live_candidate below, so this doesn't change their meaning.
    pub queuename: Option<String>,
    pub vessel: String,
    pub voyage: Option<String>,
    pub jobtype: Option<String>,
    pub jobstatus: Option<String>,
    pub yt_status: Option<String>,
    pub ytno: Option<String>,
    pub armgc: Option<String>,
    pub etw_dt: Option<String>,
    pub actv_dt: Option<String>, // JOB_ODR_ACTV_DT: order/RTG activation (soon-idle handover-start, esp. DS)
    pub upd_dt: Option<String>,  // UPD_DT (TO_CHAR'd): TOS row last-update ≈ truck-assignment time (D_tos)
    pub cre_dt: Option<String>,  // CRE_DT (TO_CHAR'd): 작업지시 생성 시각. 재고 깊이를 실측하려고 뽑는다
    /// YT_DIS_DT — TOS 가 이 트럭을 배차한 시각(권위값·mig 0148). `upd_dt` 는 대리값일 뿐이라
    /// 행이 나중에 또 갱신되면 뒤로 밀린다(실측 중앙 0초·p90 1,382초·최대 12,757초).
    /// ⚠ VARCHAR2(14) 라 TO_CHAR 를 걸지 않는다 — 이미 'YYYYMMDDHH24MISS' 문자열이다.
    pub yt_dis_dt: Option<String>,
    pub contno: Option<String>,
    pub msnseq: Option<String>,
    /// JOB_ODR_SEQNO — 크레인 작업 순번(배치 발행시각 꼴 문자열, 사전순=시간순). 구역 안 순서의
    /// 권위 값. 동률은 트윈(상자 2개·무브 1회)이다. msnseq 와 혼동 금지 — 그쪽은 항상 비어 있다.
    pub seqno: Option<String>,
    pub yt_topos: Option<String>,
    pub from_pos: Option<String>,
    pub to_pos: Option<String>,
    pub twintandem: Option<String>,
    pub twinkey: Option<String>, // twin pair grouping (same twinkey = 2 containers, 1 truck)
}

/// One row of the merged pool-tick scan (sql/pool_tick.sql): the superset of
/// QueueRow (SRC='WQ') and MoveRow (SRC='WP') columns, NULL-padded per branch.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct PoolTickRow {
    pub src: String, // 'WQ' | 'WP'
    pub queuename: Option<String>,
    pub vessel: String,
    pub voyage: Option<String>,
    // WQ branch
    pub qc: Option<String>,
    pub disload: Option<String>,
    pub seq: Option<i64>,
    pub total_qty: Option<i64>,
    pub comp_qty: Option<i64>,
    pub plan_qty: Option<i64>,
    // WP branch
    pub jobtype: Option<String>,
    pub jobstatus: Option<String>,
    pub yt_status: Option<String>,
    pub ytno: Option<String>,
    pub armgc: Option<String>,
    pub etw_dt: Option<String>,
    pub actv_dt: Option<String>,
    pub upd_dt: Option<String>,
    pub cre_dt: Option<String>,
    pub yt_dis_dt: Option<String>,
    pub contno: Option<String>,
    pub msnseq: Option<String>,
    pub seqno: Option<String>,
    pub yt_topos: Option<String>,
    pub from_pos: Option<String>,
    pub to_pos: Option<String>,
    pub twintandem: Option<String>,
    pub twinkey: Option<String>,
}

/// Split merged pool-tick rows back into the two shapes the landing code expects.
/// A WQ row with no crane can't key live_workqueue — skip it with a warning rather
/// than failing the batch (the SQL WHERE already excludes them; this is belt+braces).
fn split_pool_tick(rows: Vec<PoolTickRow>) -> (Vec<QueueRow>, Vec<MoveRow>) {
    let mut wq = Vec::new();
    let mut wp = Vec::new();
    for r in rows {
        if r.src == "WQ" {
            let (Some(qc), Some(queuename)) = (r.qc, r.queuename) else {
                tracing::warn!(vessel = %r.vessel, "pool_tick WQ row missing qc/queuename — skipped");
                continue;
            };
            wq.push(QueueRow {
                qc,
                vessel: r.vessel,
                voyage: r.voyage,
                queuename,
                disload: r.disload,
                seq: r.seq,
                total_qty: r.total_qty,
                comp_qty: r.comp_qty,
                plan_qty: r.plan_qty,
            });
        } else {
            wp.push(MoveRow {
                queuename: r.queuename,
                vessel: r.vessel,
                voyage: r.voyage,
                jobtype: r.jobtype,
                jobstatus: r.jobstatus,
                yt_status: r.yt_status,
                ytno: r.ytno,
                armgc: r.armgc,
                etw_dt: r.etw_dt,
                actv_dt: r.actv_dt,
                upd_dt: r.upd_dt,
                cre_dt: r.cre_dt,
                yt_dis_dt: r.yt_dis_dt,
                contno: r.contno,
                msnseq: r.msnseq,
                seqno: r.seqno,
                yt_topos: r.yt_topos,
                from_pos: r.from_pos,
                to_pos: r.to_pos,
                twintandem: r.twintandem,
                twinkey: r.twinkey,
            });
        }
    }
    (wq, wp)
}

/// Fetch both tick populations. Merged mode (default) = one Oracle round-trip via
/// SQL_POOL_TICK; env WORKPOOL_FETCH=split = the pre-CHUNK8 two-scan path (kill switch).
/// Logged under etl_run_log key POOL_FETCH so Oracle time/failures stay observable
/// (the WORKQUEUE/WORKPOOL runs now time only their Postgres landing).
async fn fetch_pool_tick(
    pool: &PgPool,
    target: &str,
    date: chrono::NaiveDate,
) -> Result<(Vec<QueueRow>, Vec<MoveRow>)> {
    let run_id = crate::db::start_run(pool, "POOL_FETCH", date).await?;
    let split_mode = std::env::var("WORKPOOL_FETCH").map(|v| v == "split").unwrap_or(false);
    let fetched: Result<(Vec<QueueRow>, Vec<MoveRow>)> = async {
        let tb = Toolbox::from_env(target)?;
        if split_mode {
            let wq = parse_rows(&tb.run_sql(SQL_WORKQUEUE).await?).context("parsing workqueue rows")?;
            let wp = parse_rows(&tb.run_sql(SQL_WORKPOOL).await?).context("parsing workpool rows")?;
            Ok((wq, wp))
        } else {
            let rows: Vec<PoolTickRow> =
                parse_rows(&tb.run_sql(SQL_POOL_TICK).await?).context("parsing pool_tick rows")?;
            Ok(split_pool_tick(rows))
        }
    }
    .await;
    match &fetched {
        Ok((wq, wp)) => {
            crate::db::finish_run(pool, run_id, "POOL_FETCH", date, "OK",
                Some((wq.len() + wp.len()) as i64), None).await?;
        }
        Err(e) => {
            crate::db::finish_run(pool, run_id, "POOL_FETCH", date, "FAILED",
                None, Some(&e.to_string())).await?;
        }
    }
    fetched
}

/// Parse an ETW field ("YYYYMMDDHH24MISS[mmm]", terminal MYT) to a UTC instant.
/// Returns None for empty/short/malformed values.
pub fn parse_etw(raw: &str) -> Option<DateTime<Utc>> {
    let s = raw.trim();
    if s.len() < 14 || !s.as_bytes()[..14].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(&s[..14], "%Y%m%d%H%M%S").ok()?;
    Some(tt_core::shift::terminal_to_utc(naive))
}

/// Run one work-pool tick: one Oracle fetch, then refresh the snapshot tables. Each
/// landing step is logged and a failure in one does not abort the other. A failed
/// fetch marks both landing keys FAILED (same observability as when each scan was
/// its own round-trip) and still lets the local-only ETW step run.
pub async fn tick_workpool(pool: &PgPool, target: &str) -> Result<()> {
    let date = tt_core::shift::terminal_now().date_naive();
    let as_of = Utc::now();

    macro_rules! step {
        ($name:expr, $fut:expr) => {
            if let Err(e) = $fut.await {
                tracing::error!(source = $name, error = %e, "workpool source failed (continuing)");
            }
        };
    }
    match fetch_pool_tick(pool, target, date).await {
        Ok((wq_rows, wp_rows)) => {
            step!("workqueue", src_workqueue(pool, &wq_rows, date, as_of));
            step!("workpool", src_workpool(pool, &wp_rows, date, as_of));
        }
        Err(e) => {
            let msg = e.to_string();
            for key in ["WORKQUEUE", "WORKPOOL"] {
                if let Ok(rid) = crate::db::start_run(pool, key, date).await {
                    let _ = crate::db::finish_run(pool, rid, key, date, "FAILED", None, Some(&msg)).await;
                }
            }
            tracing::error!(error = %msg, "pool_tick fetch failed (workqueue/workpool skipped this tick)");
        }
    }
    step!("etw", src_etw(pool, date));
    tracing::info!(%as_of, "workpool tick done");
    Ok(())
}

/// Accurate per-container ETW from the Azure tos_etw_gateway (TOS ETW RPC). For each active
/// voyage in the work pool, GET /v1/voyages/{vessel}/{voyage}/snapshot (via the wp-etw-bridge
/// SSH tunnel) and upsert the containers' ETW. No Oracle.
///
/// CHUNK8 (2026-08-10, gateway untouched — this is purely our fetch loop):
/// - The gateway stamps each snapshot with expires_at_utc (measured TTL 1800s, and
///   fetched_at_utc shows it serves the same cached snapshot within that window), so
///   refetching an unexpired voyage cannot return new data. Skip it. This cuts our
///   calls into the shared gateway ~20x. Kill switch: env ETW_FETCH=always.
/// - Keep the WHOLE snapshot, not just containers currently in live_workpool: a
///   container entering the pool later then already has its ETW row, which is what
///   lets the expiry gate be the only refetch trigger. (~10-15k rows steady — small.)
/// A voyage the gateway doesn't know (404) has no rows, so it is retried every tick;
/// measured 0.4s per miss, acceptable.
async fn src_etw(pool: &PgPool, date: chrono::NaiveDate) -> Result<()> {
    run_logged(pool, "ETW", date, |_| async move {
        let base = std::env::var("ETW_GATEWAY_URL").unwrap_or_else(|_| "http://127.0.0.1:18080".into());
        let honor_expiry = std::env::var("ETW_FETCH").map(|v| v != "always").unwrap_or(true);
        let voyages: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT vessel, voyage FROM live_workpool WHERE voyage IS NOT NULL AND voyage <> ''",
        ).fetch_all(pool).await?;
        // Latest stored expiry per voyage = when its snapshot stops being current.
        let mut valid_until: HashMap<(String, String), DateTime<Utc>> = HashMap::new();
        if honor_expiry {
            let rows: Vec<(String, String, Option<DateTime<Utc>>)> = sqlx::query_as(
                "SELECT vessel, voyage, max(expires_at_utc) FROM tos_etw_cntr GROUP BY 1, 2",
            ).fetch_all(pool).await?;
            for (v, voy, exp) in rows {
                if let Some(e) = exp { valid_until.insert((v, voy), e); }
            }
        }
        let parse_ts = |v: Option<&str>| v.and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&Utc));

        let now = Utc::now();
        let mut tx = pool.begin().await?;
        let mut n = 0u64;
        let (mut fetched_ct, mut skipped_ct) = (0u32, 0u32);
        for (vessel, voyage) in &voyages {
            let unexpired = valid_until
                .get(&(vessel.clone(), voyage.clone()))
                .is_some_and(|e| *e > now);
            if unexpired {
                skipped_ct += 1;
                continue;
            }
            let voye = voyage.replace('/', "%2F");
            let url = format!("{base}/v1/voyages/{vessel}/{voye}/snapshot");
            let out = tokio::process::Command::new("curl")
                .args(["-fsS", "-m", "8", &url]).output().await;
            let body = match out {
                Ok(o) if o.status.success() => o.stdout,
                _ => { tracing::warn!(%vessel, %voyage, "etw snapshot fetch failed"); continue; }
            };
            fetched_ct += 1;
            let snap: serde_json::Value = match serde_json::from_slice(&body) { Ok(v) => v, Err(_) => continue };
            let fetched = parse_ts(snap.get("fetched_at_utc").and_then(|v| v.as_str()));
            let expires = parse_ts(snap.get("expires_at_utc").and_then(|v| v.as_str()));
            for c in snap.get("cntr_list").and_then(|v| v.as_array()).into_iter().flatten() {
                let cntr = c.get("cntr_no").and_then(|v| v.as_str()).unwrap_or("");
                if cntr.is_empty() { continue; }
                let disld = c.get("dis_ld").and_then(|v| v.as_str());
                let qc = parse_ts(c.get("qc_etw_utc").and_then(|v| v.as_str()));
                let vsl = parse_ts(c.get("vessel_etw_utc").and_then(|v| v.as_str()));
                sqlx::query(
                    "INSERT INTO tos_etw_cntr
                       (vessel,voyage,cntr_no,dis_ld,qc_etw_utc,vessel_etw_utc,fetched_at_utc,expires_at_utc,updated_at)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now())
                     ON CONFLICT (vessel,voyage,cntr_no) DO UPDATE SET
                       dis_ld=EXCLUDED.dis_ld, qc_etw_utc=EXCLUDED.qc_etw_utc,
                       vessel_etw_utc=EXCLUDED.vessel_etw_utc, fetched_at_utc=EXCLUDED.fetched_at_utc,
                       expires_at_utc=EXCLUDED.expires_at_utc, updated_at=now()",
                )
                .bind(vessel).bind(voyage).bind(cntr).bind(disld).bind(qc).bind(vsl).bind(fetched).bind(expires)
                .execute(&mut *tx).await.context("upsert tos_etw_cntr")?;
                n += 1;
            }
        }
        // drop ETW for voyages no longer refreshed (left the pool >2h ago)
        sqlx::query("DELETE FROM tos_etw_cntr WHERE updated_at < now() - interval '2 hours'")
            .execute(&mut *tx).await?;
        tx.commit().await?;
        tracing::info!(fetched = fetched_ct, skipped = skipped_ct, upserts = n, "etw refresh");
        Ok(n)
    }).await.map(|_| ())
}

async fn src_workqueue(pool: &PgPool, rows: &[QueueRow], date: chrono::NaiveDate, as_of: DateTime<Utc>) -> Result<()> {
    run_logged(pool, "WORKQUEUE", date, |_| async move {
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM live_workqueue").execute(&mut *tx).await?;
        for r in rows {
            sqlx::query(
                "INSERT INTO live_workqueue
                   (qc, vessel, voyage, queuename, disload, seq, total_qty, comp_qty, plan_qty, as_of_ts)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
                 ON CONFLICT (qc, vessel, queuename) DO UPDATE SET
                   voyage=EXCLUDED.voyage, disload=EXCLUDED.disload, seq=EXCLUDED.seq,
                   total_qty=EXCLUDED.total_qty, comp_qty=EXCLUDED.comp_qty,
                   plan_qty=EXCLUDED.plan_qty, as_of_ts=EXCLUDED.as_of_ts",
            )
            .bind(&r.qc).bind(&r.vessel).bind(&r.voyage).bind(&r.queuename)
            .bind(&r.disload).bind(r.seq.map(|v| v as i32))
            .bind(r.total_qty.map(|v| v as i32)).bind(r.comp_qty.map(|v| v as i32))
            .bind(r.plan_qty.map(|v| v as i32)).bind(as_of)
            .execute(&mut *tx).await.context("insert live_workqueue")?;
        }
        tx.commit().await?;
        Ok(rows.len() as u64)
    })
    .await
    .map(|_| ())
}

/// Vessel schedule (VSB_VOYAGE) → live_vessel_schedule. The deadline source: estimated
/// work-complete / departure / berth / cut-off + actuals + planned discharge/load counts.
/// Its own subcommand + 5min timer since PLAN-extractor CHUNK7 7-1(b) (out of the 90s
/// workpool tick — 228 rows, no benefit from the 90s cadence).
pub async fn tick_vessel_schedule(pool: &PgPool, target: &str) -> Result<()> {
    let date = tt_core::shift::terminal_now().date_naive();
    let as_of = Utc::now();
    src_vessel_schedule(pool, target, date, as_of).await
}

async fn src_vessel_schedule(pool: &PgPool, target: &str, date: chrono::NaiveDate, as_of: DateTime<Utc>) -> Result<()> {
    run_logged(pool, "VESSEL_SCHEDULE", date, |_| async move {
        let raw = Toolbox::from_env(target)?.run_sql(SQL_VESSEL_SCHEDULE).await?;
        let rows: Vec<VesselScheduleRow> = parse_rows(&raw).context("parsing vessel schedule rows")?;
        let ts = |s: &Option<String>| s.as_deref().and_then(parse_etw); // YYYYMMDDHHMMSS (MYT) → UTC
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM live_vessel_schedule").execute(&mut *tx).await?;
        for r in &rows {
            sqlx::query(
                "INSERT INTO live_vessel_schedule
                   (vessel, voyage, status, berthno, estber_ts, estwkc_ts, estdep_ts, cutoff_ts,
                    actber_ts, actdep_ts, disvan, loadvan, as_of_ts)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
                 ON CONFLICT (vessel, voyage) DO UPDATE SET
                   status=EXCLUDED.status, berthno=EXCLUDED.berthno, estber_ts=EXCLUDED.estber_ts,
                   estwkc_ts=EXCLUDED.estwkc_ts, estdep_ts=EXCLUDED.estdep_ts, cutoff_ts=EXCLUDED.cutoff_ts,
                   actber_ts=EXCLUDED.actber_ts, actdep_ts=EXCLUDED.actdep_ts,
                   disvan=EXCLUDED.disvan, loadvan=EXCLUDED.loadvan, as_of_ts=EXCLUDED.as_of_ts",
            )
            .bind(&r.vessel).bind(&r.voyage).bind(&r.status).bind(&r.berthno)
            .bind(ts(&r.estber)).bind(ts(&r.estwkc)).bind(ts(&r.estdep)).bind(ts(&r.cutoff))
            .bind(ts(&r.actber)).bind(ts(&r.actdep))
            .bind(r.disvan.map(|v| v as i32)).bind(r.loadvan.map(|v| v as i32)).bind(as_of)
            .execute(&mut *tx).await.context("insert live_vessel_schedule")?;
        }
        tx.commit().await?;
        Ok(rows.len() as u64)
    })
    .await
    .map(|_| ())
}

/// Block prefix of a yard code: "10X-16" → "10X" (matches livemap's centroid keys).
fn block_prefix(s: &str) -> &str {
    s.split('-').next().unwrap_or(s).trim()
}

async fn src_workpool(pool: &PgPool, rows: &[MoveRow], date: chrono::NaiveDate, as_of: DateTime<Utc>) -> Result<()> {
    run_logged(pool, "WORKPOOL", date, |_| async move {
        // candidate (unassigned) aggregation: key = (queue, vessel, jobtype, src_block);
        // value = (count, representative rtg). Discharge groups by QC (src_block = None,
        // pickup = the crane); load groups by source block (pickup varies per container).
        // value = (truck-load count, representative rtg, twinkeys already counted in this bucket)
        let mut cand: HashMap<(String, String, String, Option<String>), (i64, Option<String>, std::collections::HashSet<String>)> =
            HashMap::new();

        // live_assigned_tt population (PLAN-extractor CHUNK7 7-1(a)): ANY row (any jobtype,
        // status A/B/Q, the SQL WHERE already bounds this) with a non-empty YTNO — the exact
        // population the old separate SQL_ASSIGNED scan (DISTINCT ytno,jobstatus) produced.
        let mut assigned: std::collections::HashSet<(String, Option<String>)> = std::collections::HashSet::new();
        for r in rows {
            let yt = r.ytno.as_deref().unwrap_or("").trim();
            if yt.is_empty() { continue; }
            assigned.insert((yt.to_string(), r.jobstatus.as_deref().map(str::trim).map(str::to_string)));
        }

        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM live_workpool").execute(&mut *tx).await?;
        sqlx::query("DELETE FROM live_candidate").execute(&mut *tx).await?;
        sqlx::query("DELETE FROM live_assigned_tt").execute(&mut *tx).await?;
        for (yt, status) in &assigned {
            sqlx::query("INSERT INTO live_assigned_tt (ytno, jobstatus, as_of_ts) VALUES ($1,$2,$3)")
                .bind(yt).bind(status)
                .bind(as_of).execute(&mut *tx).await.context("insert live_assigned_tt")?;
        }

        // live_workpool / live_candidate: DS/LD only (unchanged population — the widened
        // SQL WHERE above now also returns other jobtypes/status B, which feed ONLY
        // live_assigned_tt above, never live_workpool/live_candidate).
        let mut active = 0u64;
        for r in rows {
            let is_ds_ld = matches!(r.jobtype.as_deref(), Some("DS") | Some("LD"));
            match (is_ds_ld, r.jobstatus.as_deref()) {
                (true, Some("A")) => {
                    let etw_ts = r.etw_dt.as_deref().and_then(parse_etw);
                    // ACTV_DT/UPD_DT share the ETW timestamp shape (YYYYMMDDHH24MISS[mmm], MYT).
                    let actv_ts = r.actv_dt.as_deref().and_then(parse_etw);
                    let upd_ts = r.upd_dt.as_deref().and_then(parse_etw);
                    let cre_ts = r.cre_dt.as_deref().and_then(parse_etw);
                    // YT_DIS_DT 도 같은 시각 꼴이지만 Oracle 쪽이 VARCHAR2 라 TO_CHAR 를 안 걸었다.
                    let yt_dis_ts = r.yt_dis_dt.as_deref().and_then(parse_etw);
                    sqlx::query(
                        "INSERT INTO live_workpool
                           (queuename, vessel, voyage, jobtype, jobstatus, yt_status, ytno, armgc,
                            etw_ts, etw_raw, actv_ts, actv_raw, contno, msnseq, yt_topos, from_pos, to_pos, twintandem, as_of_ts, upd_ts, cre_ts, twinkey, seqno, yt_dis_ts)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24)",
                    )
                    .bind(&r.queuename).bind(&r.vessel).bind(&r.voyage)
                    .bind(&r.jobtype).bind(&r.jobstatus).bind(&r.yt_status).bind(&r.ytno).bind(&r.armgc)
                    .bind(etw_ts).bind(&r.etw_dt).bind(actv_ts).bind(&r.actv_dt).bind(&r.contno).bind(&r.msnseq).bind(&r.yt_topos)
                    .bind(&r.from_pos).bind(&r.to_pos).bind(&r.twintandem).bind(as_of).bind(upd_ts).bind(cre_ts).bind(&r.twinkey).bind(&r.seqno)
                    .bind(yt_dis_ts)
                    .execute(&mut *tx).await.context("insert live_workpool")?;
                    active += 1;
                }
                // unassigned demand → candidate pool (only truly unassigned: no truck yet)
                (true, Some("Q")) if r.ytno.as_deref().unwrap_or("").is_empty() => {
                    let jt = r.jobtype.clone().unwrap_or_default();
                    let src_block = if jt == "LD" {
                        r.yt_topos.as_deref().map(|t| block_prefix(t).to_string()).filter(|s| !s.is_empty())
                    } else {
                        None // discharge: pickup is the QC, not a yard block
                    };
                    let e = cand
                        .entry((r.queuename.clone().unwrap_or_default(), r.vessel.clone(), jt, src_block))
                        .or_insert((0, None, std::collections::HashSet::new()));
                    // demand = TRUCK-LOADS, not containers: a twin lift (2 containers sharing a
                    // twinkey) needs ONE truck → count each twinkey once. Non-twin rows (no twinkey)
                    // count individually. (Verified vs TOS: twinkey is the real twin pairing, not contno.)
                    let twin_dup = r.twinkey.as_deref().filter(|s| !s.is_empty()).map(|tk| !e.2.insert(tk.to_string())).unwrap_or(false);
                    if !twin_dup {
                        e.0 += 1;
                    }
                    if e.1.is_none() {
                        e.1 = r.armgc.clone().filter(|s| !s.is_empty());
                    }
                    // ALSO keep the individual unassigned container (ytno = NULL) so the UI can show
                    // a container-level future sequence, not just a per-bay count.
                    let etw_ts = r.etw_dt.as_deref().and_then(parse_etw);
                    let actv_ts = r.actv_dt.as_deref().and_then(parse_etw);
                    let upd_ts = r.upd_dt.as_deref().and_then(parse_etw);
                    let cre_ts = r.cre_dt.as_deref().and_then(parse_etw);
                    // 미배차 행이라 보통 비어 있다. 그래도 그대로 담는다 — 채워져 있는데 ytno 가
                    // 비었다면 그 자체가 관측할 값이 있는 상태다(재발행 등).
                    let yt_dis_ts = r.yt_dis_dt.as_deref().and_then(parse_etw);
                    sqlx::query(
                        "INSERT INTO live_workpool
                           (queuename, vessel, voyage, jobtype, jobstatus, yt_status, ytno, armgc,
                            etw_ts, etw_raw, actv_ts, actv_raw, contno, msnseq, yt_topos, from_pos, to_pos, twintandem, as_of_ts, upd_ts, cre_ts, twinkey, seqno, yt_dis_ts)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24)",
                    )
                    .bind(&r.queuename).bind(&r.vessel).bind(&r.voyage)
                    .bind(&r.jobtype).bind(&r.jobstatus).bind(&r.yt_status).bind(Option::<String>::None).bind(&r.armgc)
                    .bind(etw_ts).bind(&r.etw_dt).bind(actv_ts).bind(&r.actv_dt).bind(&r.contno).bind(&r.msnseq).bind(&r.yt_topos)
                    .bind(&r.from_pos).bind(&r.to_pos).bind(&r.twintandem).bind(as_of).bind(upd_ts).bind(cre_ts).bind(&r.twinkey).bind(&r.seqno)
                    .bind(yt_dis_ts)
                    .execute(&mut *tx).await.context("insert live_workpool (Q unassigned)")?;
                }
                _ => {}
            }
        }

        for ((queuename, vessel, jobtype, src_block), (n, rtg, _seen)) in &cand {
            sqlx::query(
                "INSERT INTO live_candidate (queuename, vessel, jobtype, src_block, rtg, n, as_of_ts)
                 VALUES ($1,$2,$3,$4,$5,$6,$7)",
            )
            .bind(queuename).bind(vessel).bind(jobtype).bind(src_block).bind(rtg)
            .bind(*n as i32).bind(as_of)
            .execute(&mut *tx).await.context("insert live_candidate")?;
        }

        // Attach the QC from the clean current queue snapshot (unique per vessel+queue),
        // avoiding the Oracle-side fan-out against reused historic queuenames.
        for t in ["live_workpool", "live_candidate"] {
            sqlx::query(&format!(
                "UPDATE {t} x SET qc = wq.qc
                   FROM live_workqueue wq
                  WHERE wq.vessel = x.vessel AND wq.queuename = x.queuename"
            ))
            .execute(&mut *tx).await.with_context(|| format!("attach qc to {t}"))?;
        }
        tx.commit().await?;
        tracing::info!(active, candidates = cand.len(), "workpool: active moves + candidate groups");
        Ok(rows.len() as u64)
    })
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_etw_14_and_17() {
        assert!(parse_etw("20260609094228").is_some());
        assert!(parse_etw("20260609094833726").is_some()); // trailing millis tolerated
        assert!(parse_etw("").is_none());
        assert!(parse_etw("2026").is_none());
        assert!(parse_etw("notadate012345").is_none());
    }

    #[test]
    fn splits_pool_tick_rows() {
        // one WQ + one WP row, shapes as the merged pool_tick.sql probe returned them
        // (2026-08-10 oracle-prod): numbers are JSON numbers, the other branch's columns null.
        let raw = r#"{"result":"[{\"SRC\":\"WQ\",\"QUEUENAME\":\"14D-L\",\"VESSEL\":\"EMZP\",\"VOYAGE\":\"011/2026\",\"QC\":\"C11\",\"DISLOAD\":\"L\",\"SEQ\":12,\"TOTAL_QTY\":38,\"COMP_QTY\":38,\"PLAN_QTY\":0,\"JOBTYPE\":null,\"YTNO\":null},{\"SRC\":\"WP\",\"QUEUENAME\":\"34H-D\",\"VESSEL\":\"CLOA\",\"VOYAGE\":\"12E\",\"QC\":null,\"SEQ\":null,\"TOTAL_QTY\":null,\"JOBTYPE\":\"DS\",\"JOBSTATUS\":\"A\",\"YTNO\":\"TT945\",\"ARMGC\":\"RTG122\",\"ETW_DT\":\"20260609101604681\",\"CONTNO\":\"EITU0580638\",\"SEQNO\":\"20260609094100\"}]"}"#;
        let rows: Vec<PoolTickRow> = parse_rows(raw).unwrap();
        let (wq, wp) = split_pool_tick(rows);
        assert_eq!((wq.len(), wp.len()), (1, 1));
        assert_eq!(wq[0].qc, "C11");
        assert_eq!(wq[0].queuename, "14D-L");
        assert_eq!(wq[0].total_qty, Some(38));
        assert_eq!(wp[0].jobtype.as_deref(), Some("DS"));
        assert_eq!(wp[0].ytno.as_deref(), Some("TT945"));
        assert_eq!(wp[0].seqno.as_deref(), Some("20260609094100"));
        assert!(parse_etw(wp[0].etw_dt.as_deref().unwrap()).is_some());
    }

    #[test]
    fn parses_move_rows() {
        let raw = r#"{"result":"[{\"QUEUENAME\":\"34H-D\",\"VESSEL\":\"CLOA\",\"VOYAGE\":\"12E\",\"JOBTYPE\":\"DS\",\"JOBSTATUS\":\"A\",\"YT_STATUS\":\"F\",\"YTNO\":\"TT945\",\"ARMGC\":\"RTG122\",\"ETW_DT\":\"20260609101604681\",\"ACTV_DT\":\"20260609101536\",\"CONTNO\":\"EITU0580638\",\"MSNSEQ\":null,\"YT_TOPOS\":\"08T-1011\",\"FROM_POS\":\"208\",\"TO_POS\":\"208\",\"TWINTANDEM\":null}]"}"#;
        let rows: Vec<MoveRow> = parse_rows(raw).unwrap();
        assert_eq!(rows[0].queuename.as_deref(), Some("34H-D"));
        assert_eq!(rows[0].ytno.as_deref(), Some("TT945"));
        assert!(parse_etw(rows[0].etw_dt.as_deref().unwrap()).is_some());
        assert!(parse_etw(rows[0].actv_dt.as_deref().unwrap()).is_some());
    }
}
