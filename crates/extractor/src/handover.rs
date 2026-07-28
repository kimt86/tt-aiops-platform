//! Authoritative soon-idle labels. Poll JOB_ORDER_HISTORY for newly-completed handovers
//! (JOBSTATUS='C') and land the ground-truth "truck freed" moment per truck. Incremental via
//! etl_watermark (stream='handover_label'), index-supported by IDX_JOBHIST_DATETIME on
//! (JOB_HIST_DATE||JOB_HIST_TIME). Low Oracle load: a bounded index range scan every ~60s that
//! returns only the few completions since the last poll. The DS completion event is the only
//! ground truth for soon-idle accuracy (the websocket has no RTG PLC).
//! See research/soon-idle-tos (연구 2차, 다음단계 ③).

use anyhow::{Context, Result};
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::runner::Toolbox;
use crate::workpool::parse_etw; // shared MYT "YYYYMMDDHH24MISS[mmm]" → UTC parser

const STREAM: &str = "handover_label";
const FETCH_CAP: u32 = 3000; // hard cap per poll; ~tens/min in practice, so never binds

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct HistRow {
    ytno: Option<String>,
    armgc: Option<String>,
    jobtype: Option<String>,
    contno: Option<String>,
    point: Option<i64>,
    seqno: Option<String>,
    evt: Option<String>, // JOB_HIST_DATE||JOB_HIST_TIME — the completion event timestamp
    actv_dt: Option<String>,
    dis_dt: Option<String>,
    topos: Option<String>,
}

/// One incremental poll: upsert completed (C) DS/LD handovers since the watermark as
/// authoritative labels, then advance the watermark. Logged to etl_run_log.
pub async fn tick_handover(pool: &PgPool, target: &str) -> Result<()> {
    let date = tt_core::shift::terminal_now().date_naive();
    run_logged(pool, "HANDOVER_LABEL", date, |_| async move {
        // Watermark = last completion event seen (text "YYYYMMDDHHMMSS[mmm]", chronological by
        // lexicographic order). First run: start ~10 min back so we don't backfill 15 days.
        // Upper edge of this poll's window. It bounds BOTH the Oracle read and the watermark,
        // because a future-dated key can never be walked back: etl_watermark advances with
        // GREATEST(), is read as max() over EVERY snapshot_date, and nothing prunes that table — so
        // ONE future key stalls this stream permanently and only a hand-written UPDATE recovers it
        // (reproduced in a transaction 2026-07-28: a planted '20270101120000' pinned the read there).
        // Nothing is dropped: the bound follows the wall clock, so a key merely ahead of us is read
        // on a later tick — `>=` plus ON CONFLICT already make that re-read free.
        // Measured 2026-07-28: Oracle SYSDATE matches our clock to the second, and no row carries a
        // key ahead of SYSDATE, so a zero-margin bound costs nothing today.
        // 17 chars to match this key's width (DATE||TIME carries a millisecond tail): the '999'
        // tail keeps the CURRENT second's rows inside the window instead of delaying them 1s.
        let until = format!("{}999", tt_core::shift::terminal_now().format("%Y%m%d%H%M%S"));
        // Clamping the READ is what heals an already-poisoned watermark: a future value is
        // ignored and the poll restarts from the last sane one (a bounded, deduped re-read).
        let wm: Option<String> = sqlx::query_scalar(
            "SELECT max(last_completed_at) FROM etl_watermark
              WHERE stream = $1 AND last_completed_at <= $2",
        )
        .bind(STREAM)
        .bind(&until)
        .fetch_one(pool)
        .await?;
        let wm = wm.unwrap_or_else(|| {
            (tt_core::shift::terminal_now() - chrono::Duration::minutes(10))
                .format("%Y%m%d%H%M%S")
                .to_string()
        });

        // Index-supported range scan on (JOB_HIST_DATE||JOB_HIST_TIME). '>=' + ON CONFLICT dedup
        // avoids gaps at the boundary millisecond; JOBSTATUS='C' = completion (truck freed).
        let sql = format!(
            "SELECT JOB_HIST_YTNO AS ytno, JOB_HIST_ARMGC AS armgc, JOB_HIST_JOBTYPE AS jobtype,
                    JOB_HIST_CONTNO AS contno, JOB_HIST_POINT AS point, JOB_HIST_SEQNO AS seqno,
                    JOB_HIST_DATE||JOB_HIST_TIME AS evt, JOB_HIST_ACTV_DT AS actv_dt,
                    YT_DIS_DT AS dis_dt, SUBSTR(JOB_HIST_YT_TOPOS,1,40) AS topos
               FROM TOSADM.JOB_ORDER_HISTORY
              WHERE JOB_HIST_DATE||JOB_HIST_TIME >= '{wm}'
                AND JOB_HIST_DATE||JOB_HIST_TIME <= '{until}'
                AND JOB_HIST_JOBSTATUS = 'C'
                AND JOB_HIST_JOBTYPE IN ('DS','LD')
              ORDER BY JOB_HIST_DATE||JOB_HIST_TIME
              FETCH FIRST {FETCH_CAP} ROWS ONLY"
        );
        let raw = Toolbox::from_env(target)?.run_sql(&sql).await?;
        let rows: Vec<HistRow> = parse_rows(&raw).context("parsing handover history rows")?;

        let mut tx = pool.begin().await?;
        let mut max_evt: Option<String> = None;
        let mut inserted = 0u64;
        for r in &rows {
            let (Some(contno), Some(point), Some(seqno), Some(evt)) =
                (r.contno.as_deref(), r.point, r.seqno.as_deref(), r.evt.as_deref())
            else {
                continue;
            };
            let Some(comp_ts) = parse_etw(evt) else { continue };
            let res = sqlx::query(
                "INSERT INTO tos_handover_label
                   (contno, point, seqno, ytno, armgc, jobtype, topos, dis_ts, actv_ts, comp_ts)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
                 ON CONFLICT (contno, point, seqno) DO NOTHING",
            )
            .bind(contno.trim())
            .bind(point)
            .bind(seqno.trim())
            .bind(r.ytno.as_deref().map(str::trim))
            .bind(r.armgc.as_deref().map(str::trim))
            .bind(r.jobtype.as_deref())
            .bind(r.topos.as_deref().map(str::trim))
            .bind(r.dis_dt.as_deref().and_then(parse_etw))
            .bind(r.actv_dt.as_deref().and_then(parse_etw))
            .bind(comp_ts)
            .execute(&mut *tx)
            .await
            .context("insert tos_handover_label")?;
            inserted += res.rows_affected();
            // Only a well-formed key may advance the watermark. qc_moves/rtg_moves already guard
            // their SEQNO this way; this stream did not, so a malformed value that happens to sort
            // high could park the watermark somewhere no real row will ever reach.
            let well_formed = evt.len() == 17 && evt.bytes().all(|b| b.is_ascii_digit());
            if well_formed && max_evt.as_deref().is_none_or(|m| evt > m) {
                max_evt = Some(evt.to_string());
            }
        }
        // Advance the watermark to the latest event seen (GREATEST guards against races).
        if let Some(mx) = max_evt {
            sqlx::query(
                "INSERT INTO etl_watermark (stream, snapshot_date, last_completed_at, updated_at)
                 VALUES ($1, $2, $3, now())
                 ON CONFLICT (stream, snapshot_date) DO UPDATE
                   SET last_completed_at = GREATEST(etl_watermark.last_completed_at, EXCLUDED.last_completed_at),
                       updated_at = now()",
            )
            .bind(STREAM)
            .bind(date)
            .bind(&mx)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        tracing::info!(fetched = rows.len(), inserted, "handover labels");
        Ok(rows.len() as u64)
    })
    .await
    .map(|_| ())
}
