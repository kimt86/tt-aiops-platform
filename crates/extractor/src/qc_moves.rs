//! Quay-crane (QC: C/M/Z) move stream from MCH_OPERATION → qc_move_log. Parallel to `rtg_moves`
//! (RTG/ES yard side). QC↔truck handovers are the other two of the four cycle handovers: DS pickup
//! (QC discharges ship → truck) and LD drop (truck → QC loads onto ship). Every QC move carries a
//! truck (TRK_ID 100%), so comp_ts is the physical handover completion — the Phase-2 ground truth
//! that backfills/corrects the websocket-estimated cycle timestamps. Incremental via etl_watermark
//! (stream='qc_move'). See architecture/cycle-decomposition (§5) and rtg_moves.
//!
//! ⚠⚠ ST_DT IS THE DISPATCH INSTANT, NOT A PHYSICAL START. It equals
//! JOB_ORDER_HISTORY.YT_DIS_DT to the second (measured 2026-08-03: 98.9% of DS rows, 99.6% of LD),
//! is written retroactively when the move completes, and consecutive same-crane [ST_DT, comp_ts]
//! intervals OVERLAP 90.6% of the time — impossible for a crane that lifts one box at a time.
//! Only `comp_ts` is a physical event here. Never use it as a truth label for "when did the
//! crane work this container"; mig 0113 did and had to be reverted by mig 0115. The landing
//! column was renamed `st_ts` → `dispatch_ts` (mig 0147, 2026-08-10) so the name says what the
//! value is; the crane's true start exists NOWHERE in TOS (2026-08-10 발굴조사) — estimate it
//! (sql/local/l_qc_q.sql) if you need it.
//! ⚠ This is a QC-only property. In `rtg_moves` the same ST_DT column IS a physical start
//! (intervals overlap 1.3%, comp−st median ~60s) and stays named `st_ts` there on purpose.

use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveDateTime};
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::runner::Toolbox;
use crate::workpool::parse_etw; // shared MYT "YYYYMMDDHH24MISS[mmm]" → UTC parser

const STREAM: &str = "qc_move";
const FETCH_CAP: u32 = 5000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct MoveRow {
    machno: Option<String>,
    contno: Option<String>,
    seqno: Option<String>,
    jobtype: Option<String>,
    trk_id: Option<String>,
    st_dt: Option<String>,
    comp_dt: Option<String>, // MCH_OPER_COMPDATE||MCH_OPER_COMPTIME (14-char)
    status: Option<String>,  // MCH_OPER_STATUS: F=Full / M=empty(MT)
    // Stowage label + vessel identity, read from the row we already fetch (see mig 0109). Without
    // these, the bay label survives only in 21-day sliding shadow logs and vessel/voyage only via
    // scenario.move_hist (starts 2026-07-21, DS/LD only) — which is why 85% of this table has no
    // vessel today. All three are VARCHAR2 on the Oracle side, so String is the right shape: the
    // toolbox maps NUMBER to a JSON number, and that would make parse_rows fail the whole batch.
    queuename: Option<String>, // MCH_OPER_QUEUENAME: "18H-L" = 40ft bay 18 / Hold / Load
    vessel: Option<String>,
    voyage: Option<String>,
}

/// One incremental poll: upsert quay-crane moves completed since the watermark, advance it.
pub async fn tick_qc_moves(pool: &PgPool, target: &str) -> Result<()> {
    let today = tt_core::shift::terminal_now();
    let day = today.format("%Y%m%d").to_string();
    let run_date = today.date_naive();
    run_logged(pool, "QC_MOVE", run_date, |_| async move {
        // watermark = last SEQNO seen (text "YYYYMMDDHHMMSS", = global completion order). First run:
        // start of today so we self-backfill today (FETCH_CAP per poll, ORDER BY SEQNO ASC → catches up).
        // Upper edge of this poll's window. It bounds BOTH the Oracle read and the watermark,
        // because a future-dated key can never be walked back: etl_watermark advances with
        // GREATEST(), is read as max() over EVERY snapshot_date, and nothing prunes that table — so
        // ONE future key stalls this stream permanently and only a hand-written UPDATE recovers it
        // (reproduced in a transaction 2026-07-28: a planted '20270101120000' pinned the read there).
        // Nothing is dropped: the bound follows the wall clock, so a key merely ahead of us is read
        // on a later tick — `>=` plus ON CONFLICT already make that re-read free.
        // Measured 2026-07-28: Oracle SYSDATE matches our clock to the second, and no row carries a
        // key ahead of SYSDATE, so a zero-margin bound costs nothing today.
        // 14 chars to match SEQNO's own width: a wider bound would not bite if SEQNO is numeric.
        let until = today.format("%Y%m%d%H%M%S").to_string();
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
        let wm = wm.unwrap_or_else(|| format!("{day}000000"));
        // Safety-lag: seek from wm minus 120s so a late-/out-of-order-visible row whose SEQNO sits just below
        // the advancing high-water is still re-read (ON CONFLICT dedups the tiny overlap — same cheap PK seek).
        // Closes the silent-skip hole without re-introducing a full re-scan.
        let seek_from = NaiveDateTime::parse_from_str(&wm, "%Y%m%d%H%M%S")
            .map(|t| (t - chrono::Duration::seconds(120)).format("%Y%m%d%H%M%S").to_string())
            .unwrap_or_else(|_| wm.clone());

        // Watermark on MCH_OPER_SEQNO (global-monotonic "YYYYMMDDHHMMSS" = completion order, and the
        // LEADING column of PK MCH_PK_OPERATION). `SEQNO >= wm` SEEKS via the PK (INDEX hint pins it) and
        // reads only the new tail — NO re-scan of today's rows — so poll cost is independent of frequency
        // (verified: seek ~0.8s vs a full scan ~45s). `>=` (not `>`) re-reads the watermark second so the
        // non-unique SEQNO can't skip same-second rows; ON CONFLICT dedups the tiny overlap. REGEXP/LENGTH
        // are post-filters on the small tail. Quay cranes only; every QC move has a truck → DS-pickup / LD-drop.
        let sql = format!(
            "SELECT /*+ INDEX(MCH_OPERATION MCH_PK_OPERATION) */
                    MCH_OPER_MACHNO AS machno, SUBSTR(MCH_OPER_CONTNO,1,11) AS contno,
                    MCH_OPER_SEQNO AS seqno, MCH_OPER_JOBTYPE AS jobtype, TRK_ID AS trk_id,
                    ST_DT AS st_dt, MCH_OPER_COMPDATE||MCH_OPER_COMPTIME AS comp_dt,
                    MCH_OPER_STATUS AS status, MCH_OPER_QUEUENAME AS queuename,
                    MCH_OPER_VESSEL AS vessel, MCH_OPER_VOYAGE AS voyage
               FROM TOSADM.MCH_OPERATION
              WHERE MCH_OPER_SEQNO >= '{seek_from}'
                AND MCH_OPER_SEQNO <= '{until}'
                AND REGEXP_LIKE(MCH_OPER_MACHNO, '^(C|M|Z)[0-9]')
                AND LENGTH(MCH_OPER_COMPTIME) >= 6
              ORDER BY MCH_OPER_SEQNO
              FETCH FIRST {FETCH_CAP} ROWS ONLY"
        );
        let raw = Toolbox::from_env(target)?.run_sql(&sql).await?;
        let rows: Vec<MoveRow> = parse_rows(&raw).context("parsing qc move rows")?;

        let mut tx = pool.begin().await?;
        let mut max_seqno: Option<String> = None;
        let mut inserted = 0u64;
        for r in &rows {
            let (Some(machno), Some(contno), Some(seqno), Some(comp_dt)) =
                (r.machno.as_deref(), r.contno.as_deref(), r.seqno.as_deref(), r.comp_dt.as_deref())
            else {
                continue;
            };
            let Some(comp_ts) = parse_etw(comp_dt) else { continue };
            // TOS ST_DT = 트럭 배정 시각(완료 시 소급 기입) — 크레인 시작이 아니다(mig0115).
            // 이름이 계속 사고를 불러서 컬럼을 dispatch_ts 로 개명했다(mig0147, 2026-08-10).
            // dur_s(comp−st)는 채움 중단: '무브 소요'처럼 읽히는 이름에 배정 리드가 들어 있었고,
            // 소비자가 전부 사라졌다 — learn_dispatch_lead 는 처음부터 두 컬럼을 직접 빼고
            // (mig0116 주석), scengen 은 2026-08-10 추정 시작으로 이관. 과거 행 값은 보존.
            // 배정 리드가 필요하면 comp_ts − dispatch_ts 를 직접 뺄 것.
            let dispatch_ts = r.st_dt.as_deref().and_then(parse_etw);
            let bdate = NaiveDate::parse_from_str(comp_dt.get(..8).unwrap_or(""), "%Y%m%d").unwrap_or(run_date);
            let res = sqlx::query(
                "INSERT INTO qc_move_log
                   (machno, contno, seqno, jobtype, trk_id, dispatch_ts, comp_ts, business_date, status,
                    queuename, vessel, voyage)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
                 ON CONFLICT (machno, contno, seqno) DO NOTHING",
            )
            .bind(machno.trim())
            .bind(contno.trim())
            .bind(seqno.trim())
            .bind(r.jobtype.as_deref().map(str::trim))
            .bind(r.trk_id.as_deref().map(str::trim).filter(|s| !s.is_empty()))
            .bind(dispatch_ts)
            .bind(comp_ts)
            .bind(bdate)
            .bind(r.status.as_deref().map(str::trim).filter(|s| !s.is_empty()))
            // Empty-string filter, like trk_id/status: Oracle hands back '' for unset VARCHAR2 in
            // some paths, and '' would read as a real bay label. NULL here is legitimate, not a gap —
            // AH/GI/GO moves belong to no ship bay at all (see mig 0109).
            .bind(r.queuename.as_deref().map(str::trim).filter(|s| !s.is_empty()))
            .bind(r.vessel.as_deref().map(str::trim).filter(|s| !s.is_empty()))
            .bind(r.voyage.as_deref().map(str::trim).filter(|s| !s.is_empty()))
            .execute(&mut *tx)
            .await
            .context("insert qc_move_log")?;
            inserted += res.rows_affected();
            // advance the high-water only on a well-formed 14-digit SEQNO — a malformed one would misorder the
            // lexicographic watermark and could skip/stall the stream (the row itself is still inserted above).
            let sq = seqno.trim();
            if sq.len() == 14 && sq.bytes().all(|b| b.is_ascii_digit())
                && max_seqno.as_deref().is_none_or(|m| sq > m)
            {
                max_seqno = Some(sq.to_string());
            }
        }
        if let Some(mx) = max_seqno {
            sqlx::query(
                "INSERT INTO etl_watermark (stream, snapshot_date, last_completed_at, updated_at)
                 VALUES ($1, $2, $3, now())
                 ON CONFLICT (stream, snapshot_date) DO UPDATE
                   SET last_completed_at = GREATEST(etl_watermark.last_completed_at, EXCLUDED.last_completed_at),
                       updated_at = now()",
            )
            .bind(STREAM)
            .bind(run_date)
            .bind(&mx)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        tracing::info!(fetched = rows.len(), inserted, "qc moves");
        Ok(rows.len() as u64)
    })
    .await
    .map(|_| ())
}
