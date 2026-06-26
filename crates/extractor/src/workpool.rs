//! Live work-pool snapshot extract. Two bounded Oracle scans per ~90s tick:
//!   1. JOB_QUEUE_SCHEDULE → live_workqueue (per-QC queue plan + progress).
//!   2. JOB_ORDER_LIST (A + Q) → split in Rust into:
//!        - live_workpool  (A = dispatched in-flight moves, the QC task cards)
//!        - live_candidate (Q = UNASSIGNED demand, aggregated: discharge by QC,
//!                          load by source block — the dispatch candidate pool)
//! This is the ONLY path that brings the work pool into Postgres; the API crate can't
//! reach Oracle. "Live now" (no date window) — bounded by status + recent CRE_DT to
//! keep the scan small and Oracle-friendly.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use wp_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::runner::Toolbox;

const SQL_WORKQUEUE: &str = include_str!("../sql/workqueue.sql");
const SQL_WORKPOOL: &str = include_str!("../sql/workpool.sql");
const SQL_ASSIGNED: &str = include_str!("../sql/assigned_tt.sql");
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
    pub queuename: String,
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
    pub contno: Option<String>,
    pub msnseq: Option<String>,
    pub yt_topos: Option<String>,
    pub from_pos: Option<String>,
    pub to_pos: Option<String>,
    pub twintandem: Option<String>,
    pub twinkey: Option<String>, // twin pair grouping (same twinkey = 2 containers, 1 truck)
}

/// Parse an ETW field ("YYYYMMDDHH24MISS[mmm]", terminal MYT) to a UTC instant.
/// Returns None for empty/short/malformed values.
pub fn parse_etw(raw: &str) -> Option<DateTime<Utc>> {
    let s = raw.trim();
    if s.len() < 14 || !s.as_bytes()[..14].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(&s[..14], "%Y%m%d%H%M%S").ok()?;
    Some(wp_core::shift::terminal_to_utc(naive))
}

/// Run one work-pool tick: refresh both snapshot tables. Each source is logged and a
/// failure in one does not abort the other.
pub async fn tick_workpool(pool: &PgPool, target: &str) -> Result<()> {
    let date = wp_core::shift::terminal_now().date_naive();
    let as_of = Utc::now();

    macro_rules! step {
        ($name:expr, $fut:expr) => {
            if let Err(e) = $fut.await {
                tracing::error!(source = $name, error = %e, "workpool source failed (continuing)");
            }
        };
    }
    step!("workqueue", src_workqueue(pool, target, date, as_of));
    step!("vessel_schedule", src_vessel_schedule(pool, target, date, as_of));
    step!("workpool", src_workpool(pool, target, date, as_of));
    step!("assigned", src_assigned(pool, target, date, as_of));
    step!("etw", src_etw(pool, date));
    tracing::info!(%as_of, "workpool tick done");
    Ok(())
}

/// Accurate per-container ETW from the Azure tos_etw_gateway (TOS ETW RPC). For each active
/// voyage in the work pool, GET /v1/voyages/{vessel}/{voyage}/snapshot (via the wp-etw-bridge
/// SSH tunnel) and upsert the ETW of containers we actually have in live_workpool. No Oracle.
async fn src_etw(pool: &PgPool, date: chrono::NaiveDate) -> Result<()> {
    run_logged(pool, "ETW", date, |_| async move {
        let base = std::env::var("ETW_GATEWAY_URL").unwrap_or_else(|_| "http://127.0.0.1:18080".into());
        // only keep ETW for containers we display (the active work pool) — the per-voyage
        // snapshot returns ~1000+ containers each; filtering keeps this table small.
        let pool_cntrs: std::collections::HashSet<String> = sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT contno FROM live_workpool WHERE contno IS NOT NULL AND contno <> ''",
        ).fetch_all(pool).await?.into_iter().collect();
        let voyages: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT vessel, voyage FROM live_workpool WHERE voyage IS NOT NULL AND voyage <> ''",
        ).fetch_all(pool).await?;
        let parse_ts = |v: Option<&str>| v.and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&Utc));

        let mut tx = pool.begin().await?;
        let mut n = 0u64;
        for (vessel, voyage) in &voyages {
            let voye = voyage.replace('/', "%2F");
            let url = format!("{base}/v1/voyages/{vessel}/{voye}/snapshot");
            let out = tokio::process::Command::new("curl")
                .args(["-fsS", "-m", "8", &url]).output().await;
            let body = match out {
                Ok(o) if o.status.success() => o.stdout,
                _ => { tracing::warn!(%vessel, %voyage, "etw snapshot fetch failed"); continue; }
            };
            let snap: serde_json::Value = match serde_json::from_slice(&body) { Ok(v) => v, Err(_) => continue };
            let fetched = parse_ts(snap.get("fetched_at_utc").and_then(|v| v.as_str()));
            let expires = parse_ts(snap.get("expires_at_utc").and_then(|v| v.as_str()));
            for c in snap.get("cntr_list").and_then(|v| v.as_array()).into_iter().flatten() {
                let cntr = c.get("cntr_no").and_then(|v| v.as_str()).unwrap_or("");
                if cntr.is_empty() || !pool_cntrs.contains(cntr) { continue; }
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
        // drop ETW for containers no longer in any active pool (not refreshed in 2h)
        sqlx::query("DELETE FROM tos_etw_cntr WHERE updated_at < now() - interval '2 hours'")
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(n)
    }).await.map(|_| ())
}

/// All TTs with an active assignment of ANY job type (for utilization). Refills
/// live_assigned_tt each tick. Separate from live_workpool (DS/LD only, for dispatch).
async fn src_assigned(pool: &PgPool, target: &str, date: chrono::NaiveDate, as_of: DateTime<Utc>) -> Result<()> {
    run_logged(pool, "ASSIGNED_TT", date, |_| async move {
        let raw = Toolbox::from_env(target)?.run_sql(SQL_ASSIGNED).await?;
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "UPPERCASE")]
        struct YtRow { ytno: String, jobstatus: Option<String> }
        let rows: Vec<YtRow> = parse_rows(&raw).context("parsing assigned_tt rows")?;
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM live_assigned_tt").execute(&mut *tx).await?;
        for r in &rows {
            let yt = r.ytno.trim();
            if yt.is_empty() { continue; }
            sqlx::query("INSERT INTO live_assigned_tt (ytno, jobstatus, as_of_ts) VALUES ($1,$2,$3)")
                .bind(yt).bind(r.jobstatus.as_deref().map(str::trim))
                .bind(as_of).execute(&mut *tx).await.context("insert live_assigned_tt")?;
        }
        tx.commit().await?;
        Ok(rows.len() as u64)
    }).await.map(|_| ())
}

async fn src_workqueue(pool: &PgPool, target: &str, date: chrono::NaiveDate, as_of: DateTime<Utc>) -> Result<()> {
    run_logged(pool, "WORKQUEUE", date, |_| async move {
        let raw = Toolbox::from_env(target)?.run_sql(SQL_WORKQUEUE).await?;
        let rows: Vec<QueueRow> = parse_rows(&raw).context("parsing workqueue rows")?;
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM live_workqueue").execute(&mut *tx).await?;
        for r in &rows {
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

async fn src_workpool(pool: &PgPool, target: &str, date: chrono::NaiveDate, as_of: DateTime<Utc>) -> Result<()> {
    run_logged(pool, "WORKPOOL", date, |_| async move {
        let raw = Toolbox::from_env(target)?.run_sql(SQL_WORKPOOL).await?;
        let rows: Vec<MoveRow> = parse_rows(&raw).context("parsing workpool rows")?;

        // candidate (unassigned) aggregation: key = (queue, vessel, jobtype, src_block);
        // value = (count, representative rtg). Discharge groups by QC (src_block = None,
        // pickup = the crane); load groups by source block (pickup varies per container).
        // value = (truck-load count, representative rtg, twinkeys already counted in this bucket)
        let mut cand: HashMap<(String, String, String, Option<String>), (i64, Option<String>, std::collections::HashSet<String>)> =
            HashMap::new();

        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM live_workpool").execute(&mut *tx).await?;
        sqlx::query("DELETE FROM live_candidate").execute(&mut *tx).await?;

        let mut active = 0u64;
        for r in &rows {
            match r.jobstatus.as_deref() {
                Some("A") => {
                    let etw_ts = r.etw_dt.as_deref().and_then(parse_etw);
                    // ACTV_DT/UPD_DT share the ETW timestamp shape (YYYYMMDDHH24MISS[mmm], MYT).
                    let actv_ts = r.actv_dt.as_deref().and_then(parse_etw);
                    let upd_ts = r.upd_dt.as_deref().and_then(parse_etw);
                    sqlx::query(
                        "INSERT INTO live_workpool
                           (queuename, vessel, voyage, jobtype, jobstatus, yt_status, ytno, armgc,
                            etw_ts, etw_raw, actv_ts, actv_raw, contno, msnseq, yt_topos, from_pos, to_pos, twintandem, as_of_ts, upd_ts)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)",
                    )
                    .bind(&r.queuename).bind(&r.vessel).bind(&r.voyage)
                    .bind(&r.jobtype).bind(&r.jobstatus).bind(&r.yt_status).bind(&r.ytno).bind(&r.armgc)
                    .bind(etw_ts).bind(&r.etw_dt).bind(actv_ts).bind(&r.actv_dt).bind(&r.contno).bind(&r.msnseq).bind(&r.yt_topos)
                    .bind(&r.from_pos).bind(&r.to_pos).bind(&r.twintandem).bind(as_of).bind(upd_ts)
                    .execute(&mut *tx).await.context("insert live_workpool")?;
                    active += 1;
                }
                // unassigned demand → candidate pool (only truly unassigned: no truck yet)
                Some("Q") if r.ytno.as_deref().unwrap_or("").is_empty() => {
                    let jt = r.jobtype.clone().unwrap_or_default();
                    let src_block = if jt == "LD" {
                        r.yt_topos.as_deref().map(|t| block_prefix(t).to_string()).filter(|s| !s.is_empty())
                    } else {
                        None // discharge: pickup is the QC, not a yard block
                    };
                    let e = cand
                        .entry((r.queuename.clone(), r.vessel.clone(), jt, src_block))
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
                    sqlx::query(
                        "INSERT INTO live_workpool
                           (queuename, vessel, voyage, jobtype, jobstatus, yt_status, ytno, armgc,
                            etw_ts, etw_raw, actv_ts, actv_raw, contno, msnseq, yt_topos, from_pos, to_pos, twintandem, as_of_ts, upd_ts)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)",
                    )
                    .bind(&r.queuename).bind(&r.vessel).bind(&r.voyage)
                    .bind(&r.jobtype).bind(&r.jobstatus).bind(&r.yt_status).bind(Option::<String>::None).bind(&r.armgc)
                    .bind(etw_ts).bind(&r.etw_dt).bind(actv_ts).bind(&r.actv_dt).bind(&r.contno).bind(&r.msnseq).bind(&r.yt_topos)
                    .bind(&r.from_pos).bind(&r.to_pos).bind(&r.twintandem).bind(as_of).bind(upd_ts)
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
    fn parses_move_rows() {
        let raw = r#"{"result":"[{\"QUEUENAME\":\"34H-D\",\"VESSEL\":\"CLOA\",\"VOYAGE\":\"12E\",\"JOBTYPE\":\"DS\",\"JOBSTATUS\":\"A\",\"YT_STATUS\":\"F\",\"YTNO\":\"TT945\",\"ARMGC\":\"RTG122\",\"ETW_DT\":\"20260609101604681\",\"ACTV_DT\":\"20260609101536\",\"CONTNO\":\"EITU0580638\",\"MSNSEQ\":null,\"YT_TOPOS\":\"08T-1011\",\"FROM_POS\":\"208\",\"TO_POS\":\"208\",\"TWINTANDEM\":null}]"}"#;
        let rows: Vec<MoveRow> = parse_rows(raw).unwrap();
        assert_eq!(rows[0].queuename, "34H-D");
        assert_eq!(rows[0].ytno.as_deref(), Some("TT945"));
        assert!(parse_etw(rows[0].etw_dt.as_deref().unwrap()).is_some());
        assert!(parse_etw(rows[0].actv_dt.as_deref().unwrap()).is_some());
    }
}
