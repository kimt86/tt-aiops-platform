//! Per-voyage enrichment: once per (vessel, voyage) seen in move_hist, pull vessel size + berth
//! (CDV_VESSEL + VSB_VOYAGE) and per-container attributes + ship cells (BAPLIE discharge / MOVINS
//! load) into scenario.vessel_call + scenario.container. Bounded per voyage (index on vessel/voyage,
//! FETCH-capped), rate-limited to a few voyages per tick, serialized via the isolated toolbox.
//! Dedup by presence in vessel_call, so each voyage is pulled once. Isolated + kill-switch.
//!
//! Fields are read via util::jstr (accepts string OR number) since the toolbox emits numeric-looking
//! values as JSON numbers.

use std::time::Instant;

use anyhow::Result;
use serde_json::{json, Value};
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::state::{self, Config};
use crate::toolbox::Toolbox;
use crate::util::{decode_iso, jstr, parse_cell, parse_myt};

const MAX_VOYAGES: i64 = 6; // rate-limit per tick
const FETCH_CAP: u32 = 20000; // per-voyage manifest cap (a call is a few thousand)

pub async fn run(pool: &PgPool, target: &str) -> Result<()> {
    // Local refresh first, and NOT gated on the kill switch — same rule as yard::build, because it
    // reads only our own tables and should keep working while collection is paused.
    match refresh_open_calls(pool).await {
        Ok(n) if n > 0 => tracing::info!(updated = n, "vessel_call refreshed from live schedule"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "vessel_call refresh failed"),
    }

    let cfg = state::load_config(pool).await?;
    if !cfg.enabled {
        tracing::info!("scenario collection disabled (kill switch) — skipping enrich");
        return Ok(());
    }
    let run_id = state::start_run(pool, "enrich").await?;
    match enrich(pool, run_id, target, &cfg).await {
        Ok(()) => {
            state::finish_run(pool, run_id, "done", None).await?;
        }
        Err(e) => {
            tracing::error!(error = %e, "scenario enrich failed (isolated — others unaffected)");
            let _ = state::emit(pool, run_id, "error", "enrich_failed", json!({ "error": e.to_string() })).await;
            let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
        }
    }
    Ok(())
}

/// Fill in the half of a vessel call that is not yet known when we first see it — above all the
/// ACTUAL DEPARTURE. Reads public.live_vessel_schedule, which the extractor already refreshes every
/// 90s, so this costs ZERO Oracle.
///
/// WHY THIS EXISTS. The Oracle enrich below deliberately picks only voyages absent from vessel_call
/// (`v.vessel IS NULL`), which makes the row write-once. A voyage is first seen a few hours after it
/// berths — measured average 3.6h — which is long before it leaves, so `actdep` is captured as NULL
/// and never revisited. The damage was invisible because of an accident of history: the 105 calls
/// that DO carry a departure were all enriched on 2026-07-27, in the catch-up burst after collection
/// had been stopped since 07-23, at an average of 93.4h after berthing — i.e. they had already sailed
/// when we first looked. Every call enriched live since then has actdep NULL, which reads as
/// "68 ships berthed and not one has left".
///
/// That is not a cosmetic gap. Departure time is the baseline the whole simulator is scored against
/// ("did a better dispatch finish this ship earlier"), the plan backfill's revisit-after-departure
/// clause keys on it, and the invariant test that guards the subtraction — a departed call must have
/// zero remaining — cannot run without it.
///
/// Bounded and self-limiting: only rows still missing a departure are considered, and the moment one
/// lands the row drops out of the WHERE for good. The extra predicates keep an unchanged row from
/// being rewritten every tick — a guard that is always true is how a "fill only" update turns into
/// tuple churn.
///
/// LIMIT: the live schedule holds a departed voyage for about two days (measured actdep_ts span
/// 2026-07-31 15:05 .. 08-02 21:50), so this recovers anything that left within that window. At a
/// 15-minute cadence that is never close. Calls that sailed longer ago than the window need Oracle.
async fn refresh_open_calls(pool: &PgPool) -> Result<u64> {
    let n = sqlx::query(
        "UPDATE scenario.vessel_call vc SET
            actdep      = COALESCE(s.actdep_ts, vc.actdep),
            estdep      = COALESCE(s.estdep_ts, vc.estdep),
            cutoff      = COALESCE(s.cutoff_ts, vc.cutoff),
            disvan      = COALESCE(s.disvan,    vc.disvan),
            loadvan     = COALESCE(s.loadvan,   vc.loadvan),
            enriched_at = now()
           FROM public.live_vessel_schedule s
          WHERE s.vessel = vc.vessel AND s.voyage = vc.voyage
            AND vc.actdep IS NULL
            AND (s.actdep_ts IS NOT NULL
              OR s.estdep_ts  IS DISTINCT FROM vc.estdep
              OR s.cutoff_ts  IS DISTINCT FROM vc.cutoff
              OR s.disvan     IS DISTINCT FROM vc.disvan
              OR s.loadvan    IS DISTINCT FROM vc.loadvan)",
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n)
}

async fn enrich(pool: &PgPool, run_id: i64, target: &str, cfg: &Config) -> Result<()> {
    state::set_phase(pool, run_id, "extract").await?;

    let voyages: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT m.vessel, m.voyage
           FROM scenario.move_hist m
           LEFT JOIN scenario.vessel_call v ON v.vessel = m.vessel AND v.voyage = m.voyage
          WHERE m.vessel IS NOT NULL AND m.voyage IS NOT NULL AND v.vessel IS NULL
          LIMIT $1",
    )
    .bind(MAX_VOYAGES)
    .fetch_all(pool)
    .await?;

    let tb = Toolbox::from_env(target, cfg.oracle_timeout_s as u64)?;
    let t0 = Instant::now();
    let (mut n_voy, mut n_cont) = (0u64, 0u64);

    for (vessel, voyage) in &voyages {
        // 1) vessel size + berth
        let vsql = format!(
            "SELECT v.CDV_VSL_NAME AS name, v.CDV_VSL_LENGTH AS loa, v.CDV_VSL_WIDTH AS beam,
                    v.CDV_VSL_DRAFT AS draft, v.CDV_VSL_MAXTEU AS maxteu, v.CDV_VSL_TOTALBAY AS bays,
                    s.VSB_VOY_BERTHNO AS berthno, s.VSB_VOY_BERTHSIDE AS berthside,
                    TO_CHAR(s.VSB_VOY_STARTPOS) AS startpos,
                    s.VSB_VOY_ACTBER_DATE||s.VSB_VOY_ACTBER_TIME AS actber,
                    s.VSB_VOY_ACTDEP_DATE||s.VSB_VOY_ACTDEP_TIME AS actdep,
                    s.VSB_VOY_ESTDEP_DATE||s.VSB_VOY_ESTDEP_TIME AS estdep,
                    s.VSB_VOY_CUTOFF_DATE||s.VSB_VOY_CUTOFF_TIME AS cutoff,
                    s.VSB_VOY_DISVAN AS disvan, s.VSB_VOY_LOADVAN AS loadvan
               FROM TOSADM.CDV_VESSEL v
               LEFT JOIN TOSADM.VSB_VOYAGE s
                 ON s.VSB_VOY_VESSEL = v.CDV_VSL_CODE AND s.VSB_VOY_VOYAGE = '{voyage}'
              WHERE v.CDV_VSL_CODE = '{vessel}'
              FETCH FIRST 1 ROWS ONLY"
        );
        let vrows: Vec<Value> = parse_rows(&tb.run_sql(&vsql).await?)?;
        let vr = vrows.first();
        let vg = |k: &str| vr.and_then(|r| jstr(r, k));
        let vf = |k: &str| vg(k).and_then(|s| s.parse::<f64>().ok());
        let vi = |k: &str| vg(k).and_then(|s| s.parse::<i32>().ok());
        let vt = |k: &str| vg(k).as_deref().and_then(parse_myt);

        let mut tx = pool.begin().await?;
        sqlx::query(
            "INSERT INTO scenario.vessel_call
               (vessel,voyage,vsl_name,loa_m,beam_m,draft_m,max_teu,total_bays,berthno,berthside,
                startpos_m,actber,actdep,estdep,cutoff,disvan,loadvan,enriched_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,now())
             ON CONFLICT (vessel,voyage) DO UPDATE SET
               vsl_name=EXCLUDED.vsl_name, loa_m=EXCLUDED.loa_m, total_bays=EXCLUDED.total_bays,
               berthno=EXCLUDED.berthno, berthside=EXCLUDED.berthside, startpos_m=EXCLUDED.startpos_m,
               actber=EXCLUDED.actber, actdep=EXCLUDED.actdep, estdep=EXCLUDED.estdep,
               cutoff=EXCLUDED.cutoff, enriched_at=now()",
        )
        .bind(vessel).bind(voyage)
        .bind(vg("NAME")).bind(vf("LOA")).bind(vf("BEAM")).bind(vf("DRAFT"))
        .bind(vi("MAXTEU")).bind(vi("BAYS")).bind(vg("BERTHNO")).bind(vg("BERTHSIDE"))
        .bind(vf("STARTPOS")).bind(vt("ACTBER")).bind(vt("ACTDEP")).bind(vt("ESTDEP"))
        .bind(vt("CUTOFF")).bind(vi("DISVAN")).bind(vi("LOADVAN"))
        .execute(&mut *tx)
        .await?;

        // 2) BAPLIE (discharge) + 3) MOVINS (load)
        let bap = format!(
            "SELECT ETV_CBAP_CONTNO AS contno, ETV_CBAP_CONTISO AS iso, ETV_CBAP_FULLEMPT AS fe,
                    ETV_CBAP_CONTTYPE AS ctype, ETV_CBAP_CONTSTWG AS stwg,
                    COALESCE(NULLIF(TRIM(MEAS_WGT),''), NULLIF(TRIM(ETV_CBAP_GROSWGHT),'')) AS gross,
                    ETV_CBAP_TMPRCONT AS temp, ETV_CBAP_DNGRIMDG AS imdg, ETV_CBAP_DNGRUNNO AS unno,
                    ETV_CBAP_DCHGPORT AS pod, ETV_CBAP_ORGNPORT AS pol, ETV_CBAP_CONTOPER AS oper,
                    ETV_CBAP_OVERHIGH AS ohgt, ETV_CBAP_NEXTVESSEL AS outv, ETV_CBAP_NEXTVOYAGE AS outvoy
               FROM TOSADM.ETV_BAPLIE_CONT
              WHERE ETV_CBAP_VESSEL='{vessel}' AND ETV_CBAP_VOYAGE='{voyage}'
              FETCH FIRST {FETCH_CAP} ROWS ONLY"
        );
        let mov = format!(
            "SELECT ETV_CMOV_CONTNO AS contno, ETV_CMOV_CONTISO AS iso, ETV_CMOV_FULLEMPT AS fe,
                    ETV_CMOV_CONTTYPE AS ctype, ETV_CMOV_CONTSTWG AS stwg,
                    COALESCE(NULLIF(TRIM(MVNS_ORG_WGT),''), NULLIF(TRIM(ETV_CMOV_GROSWGHT),'')) AS gross,
                    ETV_CMOV_TMPRCONT AS temp, ETV_CMOV_DNGRIMDG AS imdg, ETV_CMOV_DNGRUNNO AS unno,
                    ETV_CMOV_DCHGPORT AS pod, ETV_CMOV_LOADPORT AS pol, ETV_CMOV_OPERATOR AS oper,
                    ETV_CMOV_OVERHIGH AS ohgt
               FROM TOSADM.ETV_MOVINS_STOWAGE
              WHERE ETV_CMOV_VESSEL='{vessel}' AND ETV_CMOV_VOYAGE='{voyage}'
              FETCH FIRST {FETCH_CAP} ROWS ONLY"
        );
        let drows: Vec<Value> = parse_rows(&tb.run_sql(&bap).await?)?;
        let lrows: Vec<Value> = parse_rows(&tb.run_sql(&mov).await?)?;

        for (disload, rows) in [("D", &drows), ("L", &lrows)] {
            for r in rows {
                let Some(contno) = jstr(r, "CONTNO") else { continue };
                let iso = jstr(r, "ISO").unwrap_or_default();
                let (size, height, iso_fam) = decode_iso(&iso);
                let family = match jstr(r, "CTYPE").as_deref() {
                    Some("RE") => "reefer",
                    Some("CT") => "tank",
                    _ => iso_fam,
                };
                let fill = match jstr(r, "FE").as_deref() {
                    Some("F") => "full",
                    _ => "empty",
                };
                let (bay, srow, tier) = parse_cell(&jstr(r, "STWG").unwrap_or_default());
                let oog = jstr(r, "OHGT").map(|x| !x.chars().all(|c| c == '0')).unwrap_or(false);
                let gross = jstr(r, "GROSS").and_then(|s| s.parse::<f64>().ok()).map(|v| v.round() as i32);
                sqlx::query(
                    "INSERT INTO scenario.container
                       (vessel,voyage,contno,disload,iso,size,height,family,fill,gross_kg,reefer_temp,
                        imdg,un_no,oog,pod,pol,operator,ship_bay,ship_row,ship_tier,out_vessel,out_voyage)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22)
                     ON CONFLICT (vessel,voyage,contno,disload) DO NOTHING",
                )
                .bind(vessel).bind(voyage).bind(&contno).bind(disload)
                .bind(if iso.is_empty() { None } else { Some(iso.as_str()) })
                .bind(size).bind(height).bind(family).bind(fill).bind(gross)
                .bind(jstr(r, "TEMP")).bind(jstr(r, "IMDG")).bind(jstr(r, "UNNO")).bind(oog)
                .bind(jstr(r, "POD")).bind(jstr(r, "POL")).bind(jstr(r, "OPER"))
                .bind(bay).bind(srow).bind(tier)
                .bind(jstr(r, "OUTV")).bind(jstr(r, "OUTVOY"))
                .execute(&mut *tx)
                .await?;
                n_cont += 1;
            }
        }
        tx.commit().await?;
        n_voy += 1;
    }

    let ms = t0.elapsed().as_millis() as i64;
    state::merge_json(pool, run_id, "load_stats", json!({
        "queries": voyages.len() * 3, "elapsed_ms": ms, "oracle_timeout_s": cfg.oracle_timeout_s,
    })).await?;
    state::merge_json(pool, run_id, "collection", json!({
        "voyages_enriched": n_voy, "containers": n_cont,
    })).await?;
    tracing::info!(voyages = n_voy, containers = n_cont, ms, "scenario enrich");
    Ok(())
}
