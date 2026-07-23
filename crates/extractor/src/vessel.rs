//! Current-shift vessel panel. `write_vessel_shift` folds the K_MPH shift rows
//! (already fetched — zero extra Oracle query) into per-vessel rows. `extract_voyage_plan`
//! pulls the planned container count (VAN) for the progress bar (TRIVIAL, slow-changing).

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use std::collections::BTreeMap;
use tt_core::shift::Shift;

use crate::kpis::common::run_logged;
use crate::kpis::k_mph_realtime::Row as MphRow;
use crate::{params, runner::Toolbox};

const SQL_VOYAGE_PLAN: &str = include_str!("../sql/voyage_plan.sql");

/// Group the current-shift K_MPH rows by vessel/voyage and replace `vessel_shift`
/// for this (date, shift).
pub async fn write_vessel_shift(
    pool: &PgPool,
    date: NaiveDate,
    sh: Shift,
    end: chrono::NaiveDateTime,
    rows: &[MphRow],
) -> Result<()> {
    struct Agg {
        moves: f64,
        load: f64,
        discharge: f64,
        active_hours: f64,
        qcs: std::collections::BTreeSet<String>,
        first: Option<String>,
        last: Option<String>,
    }
    let mut by: BTreeMap<(String, String), Agg> = BTreeMap::new();
    for r in rows {
        let e = by.entry((r.vessel.clone(), r.voyage.clone())).or_insert_with(|| Agg {
            moves: 0.0, load: 0.0, discharge: 0.0, active_hours: 0.0,
            qcs: Default::default(), first: None, last: None,
        });
        e.moves += r.moves.unwrap_or(0.0);
        e.load += r.load_moves.unwrap_or(0.0);
        e.discharge += r.discharge_moves.unwrap_or(0.0);
        e.active_hours += r.active_hours.unwrap_or(0.0);
        e.qcs.insert(r.qc_machno.clone());
        if let Some(f) = &r.first_move { if e.first.as_ref().map_or(true, |x| f < x) { e.first = Some(f.clone()); } }
        if let Some(l) = &r.last_move { if e.last.as_ref().map_or(true, |x| l > x) { e.last = Some(l.clone()); } }
    }

    // per-(vessel, voyage, qc) aggregate for the vessel-grouped QC cards
    struct QcAgg { moves: f64, load: f64, discharge: f64, active_hours: f64, first: Option<String>, last: Option<String> }
    let mut by_qc: BTreeMap<(String, String, String), QcAgg> = BTreeMap::new();
    for r in rows {
        let e = by_qc.entry((r.vessel.clone(), r.voyage.clone(), r.qc_machno.clone()))
            .or_insert_with(|| QcAgg { moves: 0.0, load: 0.0, discharge: 0.0, active_hours: 0.0, first: None, last: None });
        e.moves += r.moves.unwrap_or(0.0);
        e.load += r.load_moves.unwrap_or(0.0);
        e.discharge += r.discharge_moves.unwrap_or(0.0);
        e.active_hours += r.active_hours.unwrap_or(0.0);
        if let Some(f) = &r.first_move { if e.first.as_ref().map_or(true, |x| f < x) { e.first = Some(f.clone()); } }
        if let Some(l) = &r.last_move { if e.last.as_ref().map_or(true, |x| l > x) { e.last = Some(l.clone()); } }
    }

    let as_of = tt_core::shift::terminal_to_utc(end);
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM vessel_shift WHERE business_date=$1 AND shift=$2")
        .bind(date).bind(sh.label()).execute(&mut *tx).await?;
    for ((vessel, voyage), a) in by {
        let mph = if a.active_hours > 0.0 { Some((a.moves / a.active_hours * 100.0).round() / 100.0) } else { None };
        let qcs = a.qcs.iter().cloned().collect::<Vec<_>>().join(",");
        sqlx::query(
            "INSERT INTO vessel_shift
               (business_date, shift, vessel, voyage, moves, load_moves, discharge_moves,
                qc_count, qcs, mph, first_move, last_move, as_of_ts)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(date).bind(sh.label()).bind(&vessel).bind(&voyage)
        .bind(a.moves as i32).bind(a.load as i32).bind(a.discharge as i32)
        .bind(a.qcs.len() as i32).bind(&qcs).bind(mph)
        .bind(&a.first).bind(&a.last).bind(as_of)
        .execute(&mut *tx).await.context("vessel_shift insert")?;
    }

    sqlx::query("DELETE FROM vessel_qc_shift WHERE business_date=$1 AND shift=$2")
        .bind(date).bind(sh.label()).execute(&mut *tx).await?;
    for ((vessel, voyage, qc), a) in by_qc {
        let mph = if a.active_hours > 0.0 { Some((a.moves / a.active_hours * 100.0).round() / 100.0) } else { None };
        sqlx::query(
            "INSERT INTO vessel_qc_shift
               (business_date, shift, vessel, voyage, qc, moves, load_moves, discharge_moves,
                active_hours, mph, first_move, last_move, as_of_ts)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(date).bind(sh.label()).bind(&vessel).bind(&voyage).bind(&qc)
        .bind(a.moves as i32).bind(a.load as i32).bind(a.discharge as i32)
        .bind((a.active_hours * 100.0).round() / 100.0).bind(mph)
        .bind(&a.first).bind(&a.last).bind(as_of)
        .execute(&mut *tx).await.context("vessel_qc_shift insert")?;
    }
    tx.commit().await?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct PlanRow {
    vessel: String,
    voyage: String,
    planned_moves: Option<f64>,
}

/// Pull planned container counts (VAN) for recent voyages into raw_voyage_plan.
pub async fn extract_voyage_plan(pool: &PgPool, target: &str, date: NaiveDate) -> Result<()> {
    run_logged(pool, "VOYAGE_PLAN", date, |run_id| async move {
        let sql = params::render_window(SQL_VOYAGE_PLAN, date, 3)?; // last 3 days
        let raw = Toolbox::from_env(target)?.run_sql(&sql).await?;
        let rows: Vec<PlanRow> = tt_core::parse::parse_rows(&raw)?;
        let mut tx = pool.begin().await?;
        for r in &rows {
            sqlx::query(
                "INSERT INTO raw_voyage_plan (vessel, voyage, planned_moves, source, run_id)
                 VALUES ($1,$2,$3,'VSS_STT_VAN',$4)
                 ON CONFLICT (vessel, voyage) DO UPDATE SET
                   planned_moves=EXCLUDED.planned_moves, run_id=EXCLUDED.run_id, extracted_at=now()",
            )
            .bind(&r.vessel).bind(&r.voyage).bind(r.planned_moves.map(|v| v as i32)).bind(run_id)
            .execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(rows.len() as u64)
    }).await.map(|_| ())
}
