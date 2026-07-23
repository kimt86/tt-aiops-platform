//! LIVE tab: current-shift cumulative KPIs (vs the previous shift at the same elapsed
//! minutes) and the current-shift vessel panel. Reads Postgres only. Current shift is
//! resolved in the TERMINAL timezone (MYT), not the server clock.

use axum::{extract::State, Json};
use chrono::NaiveDate;
use sqlx::PgPool;
use tt_core::shift;

use crate::models::*;
use crate::routes::{load_targets, AppError, ORDER};

pub async fn live(State(pool): State<PgPool>) -> Result<Json<LiveResponse>, AppError> {
    let now = shift::terminal_now().naive_local();
    let (date, sh) = shift::current(now);
    let (start, nominal_end) = shift::window(date, sh);
    let as_of = now.min(nominal_end);
    let elapsed_min = (as_of - start).num_minutes().max(0);
    let remaining_min = (nominal_end - as_of).num_minutes().max(0);
    let (prev_date, prev_sh) = shift::previous(date, sh);

    // current-shift values
    let cur: Vec<(String, Option<f64>, Option<i32>)> = sqlx::query_as(
        "SELECT kpi_key, value::float8, sample_n FROM kpi_shift WHERE business_date=$1 AND shift=$2",
    )
    .bind(date)
    .bind(sh.label())
    .fetch_all(&pool)
    .await?;

    // previous-shift history (pick the row closest to the current elapsed minutes)
    let prev_hist: Vec<(String, Option<f64>, i32)> = sqlx::query_as(
        "SELECT kpi_key, value::float8, elapsed_min FROM kpi_shift_history WHERE business_date=$1 AND shift=$2",
    )
    .bind(prev_date)
    .bind(prev_sh.label())
    .fetch_all(&pool)
    .await?;

    let targets = load_targets(&pool).await?;
    let cur_v = |k: &str| cur.iter().find(|r| r.0 == k);
    let prev_v = |k: &str| -> Option<f64> {
        prev_hist
            .iter()
            .filter(|r| r.0 == k)
            .min_by_key(|r| (r.2 as i64 - elapsed_min).abs())
            .and_then(|r| r.1)
    };

    let mut kpis = Vec::new();
    for kpi in ORDER {
        let key = kpi.as_str();
        let value = cur_v(key).and_then(|r| r.1);
        let sample_n = cur_v(key).and_then(|r| r.2);
        let prev_value = prev_v(key);
        let (delta_abs, delta_pct) = match (value, prev_value) {
            (Some(v), Some(p)) if p != 0.0 => (Some(v - p), Some((v - p) / p * 100.0)),
            (Some(v), Some(p)) => (Some(v - p), None),
            _ => (None, None),
        };
        let tgt = targets.get(key);
        let dir = tgt.and_then(|t| t.direction.as_deref());
        let meets_target = match (value, tgt.and_then(|t| t.target), dir) {
            (Some(v), Some(th), Some(d)) => Some(if d == "LOWER_BETTER" { v <= th } else { v >= th }),
            _ => None,
        };
        // per-jobtype cycle (DS/LD) for the cycle card — today's split
        let (ds_cycle_s, ld_cycle_s) = if key == "K_CYCLE" {
            crate::routes::cycle_by_jobtype(&pool, date, date).await.unwrap_or((None, None))
        } else {
            (None, None)
        };
        kpis.push(LiveKpi {
            key: key.to_string(),
            name_en: kpi.name_en().to_string(),
            name_ko: kpi.name_ko().to_string(),
            unit: kpi.unit().to_string(),
            tier: tgt.and_then(|t| t.tier.clone()),
            direction: tgt.and_then(|t| t.direction.clone()),
            value,
            sample_n,
            prev_value,
            delta_abs,
            delta_pct,
            target: tgt.and_then(|t| t.target),
            excellent: tgt.and_then(|t| t.excellent),
            meets_target,
            ds_cycle_s,
            ld_cycle_s,
        });
    }

    Ok(Json(LiveResponse {
        business_date: date.to_string(),
        shift: sh.label().to_string(),
        shift_name_ko: sh.name_ko().to_string(),
        shift_name_en: sh.name_en().to_string(),
        window_start: start.format("%H:%M").to_string(),
        as_of: as_of.format("%H:%M").to_string(),
        elapsed_min,
        remaining_min,
        prev_shift: prev_sh.label().to_string(),
        kpis,
    }))
}

pub async fn vessels(State(pool): State<PgPool>) -> Result<Json<VesselsResponse>, AppError> {
    let now = shift::terminal_now().naive_local();
    let (date, sh): (NaiveDate, _) = shift::current(now);

    let rows: Vec<(String, String, Option<i32>, Option<i32>, Option<i32>, Option<i32>, Option<String>, Option<f64>, Option<String>, Option<String>, Option<i32>)> =
        sqlx::query_as(
            "SELECT v.vessel, v.voyage, v.moves, v.load_moves, v.discharge_moves, v.qc_count,
                    v.qcs, v.mph::float8, v.first_move, v.last_move, p.planned_moves
               FROM vessel_shift v
               LEFT JOIN raw_voyage_plan p ON p.vessel=v.vessel AND p.voyage=v.voyage
              WHERE v.business_date=$1 AND v.shift=$2
              ORDER BY v.moves DESC NULLS LAST",
        )
        .bind(date)
        .bind(sh.label())
        .fetch_all(&pool)
        .await?;

    // per-(vessel, voyage, qc) throughput for the vessel-grouped QC cards
    let qc_rows: Vec<(String, String, String, Option<i32>, Option<i32>, Option<i32>, Option<f64>)> =
        sqlx::query_as(
            "SELECT vessel, voyage, qc, moves, load_moves, discharge_moves, mph::float8
               FROM vessel_qc_shift
              WHERE business_date=$1 AND shift=$2
              ORDER BY moves DESC NULLS LAST",
        )
        .bind(date)
        .bind(sh.label())
        .fetch_all(&pool)
        .await?;
    let mut qc_by: std::collections::HashMap<(String, String), Vec<VesselQc>> = std::collections::HashMap::new();
    for (vessel, voyage, qc, mv, ld, ds, mph) in qc_rows {
        qc_by.entry((vessel, voyage)).or_default().push(VesselQc {
            qc, moves: mv, load_moves: ld, discharge_moves: ds, mph,
        });
    }

    let vessels = rows
        .into_iter()
        .map(|(vessel, voyage, moves, ld, ds, qcc, qcs, mph, fm, lm, planned)| {
            let qc_list = qcs
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect::<Vec<_>>();
            let progress_pct = match (moves, planned) {
                (Some(m), Some(p)) if p > 0 => Some((m as f64 / p as f64 * 100.0 * 10.0).round() / 10.0),
                _ => None,
            };
            let qc_rows = qc_by.remove(&(vessel.clone(), voyage.clone())).unwrap_or_default();
            VesselRow {
                vessel,
                voyage,
                qcs: qc_list,
                qc_count: qcc,
                moves,
                load_moves: ld,
                discharge_moves: ds,
                mph,
                first_move: fm,
                last_move: lm,
                planned_moves: planned,
                progress_pct,
                qc_rows,
            }
        })
        .collect();

    Ok(Json(VesselsResponse {
        shift: sh.label().to_string(),
        as_of: now.format("%H:%M").to_string(),
        vessels,
    }))
}
