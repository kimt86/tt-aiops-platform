//! 적부계획의 상자별 작업 순번(TOS `VSP_SHIP.VSP_SHP_PLANSEQ`) → `live_stow_plan`.
//!
//! **왜 있나.** 구역 안에서 "이 상자가 몇 번째인가"를 우리는 작업지시 생성시각으로 추정하고
//! 있었는데, 작업지시 표에는 순서가 없다 — `MSNSEQ` 는 전부 빔, `SEQNO` 는 발행 시각이며
//! 완료되면 완료 시각으로 덮어쓰인다(끝난 작업으로 채점하면 100% 로 보이는 함정). 그래서 상자
//! 79.5% 가 다른 상자와 같은 값을 공유했고 순번이 사실상 임의였다.
//!
//! **TOS 자신은** ITV 배차기에서 이렇게 정렬한다 —
//! `ORDER BY JOB_QUE_PLND_DATE||TIME, VSP_SHP_PLANSEQ` (LoadableJob.xml). 순서는 작업지시가
//! 아니라 적부계획에 있다.
//!
//! **알갱이(실측 2026-08-05).** 적·양하 합쳐 구역 164곳 **전부**에서 순번이 상자를 완전히
//! 구별한다(값 하나당 1상자). 지금 쓰는 축(작업지시 seqno)은 17곳 중 1곳뿐이다.
//!
//! ⚠ **`PLANST='P'` 인 행만 쓴다.** 'B'(BAPLIE 신고분)에는 순번이 없다. 그걸 분모에 넣으면
//!   "양하는 13.6% 뿐"이라는 오판이 나온다 — mig0128 에서 내가 그렇게 틀렸고 0129 에서 정정했다.
//!   P 행 수는 실측상 큐 카운터의 남은 일과 일치한다.
//!
//! **모드 둘 (`STOWPLAN_MODE`, 기본 `delta`, 킬스위치).**
//! - `snapshot`: 옛 방식. 매 주기 VSP_SHIP 을 통째로 다시 읽어 `live_stow_plan` 을 DELETE+INSERT
//!   전체교체(6,200행×5분 ≈ 74k행/h — 이 추출기 최대 전송원).
//! - `delta`(기본, mig 0135): VSP_SHIP.UPD_DT(`IDX_VSP_SHIP_UPD_DT`) 로 바뀐 행만 받아 거울에
//!   병합한다. 완료(COMPDATE NOT NULL)는 삭제, 그 외는 UNIQUE 키(vessel,voyage,contno,disload)
//!   기준 UPSERT — 계획 개정으로 queuename(구역)이 바뀌어도 같은 상자로 인식해 갱신한다(옛 PK
//!   (vessel,voyage,queuename,contno) 로 하면 구역 이동이 새 행으로 남아 유령이 된다).
//!   워터마크 없는 첫 델타 틱은 이 항차 범위를 스냅샷처럼 한 번 정리(scoped DELETE)하고 돈다.
//!   드리프트(범위 밖으로 빠진 행 등)는 `tt-stowplan-recon`(1시간)이 스냅샷으로 자가치유한다.

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::runner::Toolbox;

const SQL_STOWPLAN: &str = include_str!("../sql/stowplan.sql");
const SQL_STOWPLAN_DELTA: &str = include_str!("../sql/stowplan_delta.sql");
const SQL_STOWPLAN_SEED: &str = include_str!("../sql/stowplan_seed.sql");

/// 한 주기에 조회할 항차 수 상한. 넘으면 자르고 경고한다 — 조용히 줄이면 "다 봤다"로 읽힌다.
/// 실측: 항차 26개·미완료 적하 4,725행에 2.3초. 40이면 배 이상 여유다.
const MAX_VOYAGES: usize = 40;

const WM_STREAM: &str = "stowplan_delta";
/// 워터마크가 아직 없는 첫 델타 틱에 쓰는 바닥값 — 사실상 "지금 범위의 전부"를 받는다.
const WM_FLOOR: &str = "19700101000000";
/// 워터마크 기록 시 뒤로 물러서는 안전랙(초). 경계 초에 늦게 보인 행을 다음 틱이 다시 본다
/// (거울 병합은 UPSERT/삭제 둘 다 멱등이라 재처리 비용이 없다).
const SAFETY_LAG_S: i64 = 120;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct PlanRow {
    pub vessel: String,
    pub voyage: String,
    pub queuename: Option<String>,
    /// 'D'=양하 · 'L'=적하. 구역 이름(38H-D 등)으로 유추하지 않고 값으로 구분한다.
    pub disload: Option<String>,
    pub contno: Option<String>,
    /// NUMBER 라 툴박스가 JSON 숫자로 준다. `Option<String>` 으로 받으면 배치째 디코드가 깨진다.
    pub planseq: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct PlanDeltaRow {
    vessel: String,
    voyage: String,
    queuename: Option<String>,
    disload: Option<String>,
    contno: Option<String>,
    planseq: Option<i64>,
    /// VSP_SHP_COMPDATE. NOT NULL(비어있지 않음) = 완료 → 거울에서 지운다.
    compdate: Option<String>,
    /// TO_CHAR(UPD_DT,'YYYYMMDDHH24MISS') — 워터마크 전진용.
    upd: Option<String>,
}

pub async fn tick_stowplan(pool: &PgPool, target: &str) -> Result<()> {
    let date = tt_core::shift::terminal_now().date_naive();
    let as_of = Utc::now();
    let mode = std::env::var("STOWPLAN_MODE").unwrap_or_else(|_| "delta".to_string());
    if mode == "snapshot" {
        src_stowplan_snapshot(pool, target, date, as_of).await
    } else {
        src_stowplan_delta(pool, target, date, as_of).await
    }
}

/// 지금 크레인이 붙어 실제로 작업이 진행중인 항차만. Oracle 부하 0(우리 Postgres 에서 만든다).
async fn active_voyages(pool: &PgPool) -> Result<Vec<(String, String)>> {
    let voyages: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT q.vessel, q.voyage
           FROM live_workqueue q
          WHERE q.qc IS NOT NULL AND q.qc <> ''
            AND q.voyage IS NOT NULL AND q.voyage <> ''
            AND (q.total_qty - q.comp_qty) > 0
            AND (q.vessel, q.voyage) IN (
                  SELECT vessel, voyage FROM live_workqueue WHERE comp_qty > 0)
          ORDER BY 1, 2",
    )
    .fetch_all(pool)
    .await?;
    Ok(voyages)
}

fn voyage_in_list(used: &[(String, String)]) -> String {
    let esc = |s: &str| s.replace('\'', "''");
    used.iter()
        .map(|(v, y)| format!("('{}','{}')", esc(v), esc(y)))
        .collect::<Vec<_>>()
        .join(",")
}

/// 옛 방식: 매 주기 전체를 다시 읽어 통째로 바꾼다(`STOWPLAN_MODE=snapshot`).
async fn src_stowplan_snapshot(
    pool: &PgPool,
    target: &str,
    date: chrono::NaiveDate,
    as_of: DateTime<Utc>,
) -> Result<()> {
    run_logged(pool, "STOWPLAN", date, |_| async move {
        let voyages = active_voyages(pool).await?;
        if voyages.is_empty() {
            tracing::info!("stowplan: 작업중인 항차 없음 — Oracle 조회 생략");
            return Ok(0);
        }
        let total = voyages.len();
        let used: Vec<_> = voyages.into_iter().take(MAX_VOYAGES).collect();
        if total > used.len() {
            tracing::warn!(total, used = used.len(), "stowplan: 항차가 상한을 넘어 잘랐다");
        }
        let list = voyage_in_list(&used);
        let sql = SQL_STOWPLAN.replace("__VOYAGES__", &list);

        let raw = Toolbox::from_env(target)?.run_sql(&sql).await?;
        let rows: Vec<PlanRow> = parse_rows(&raw).context("parsing stowplan rows")?;

        // 스냅샷 — 계획은 개정되므로 통째로 바꾼다. 한 트랜잭션 안이라 조회자가 빈 표를 보는
        // 순간은 없다.
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM live_stow_plan").execute(&mut *tx).await?;
        let mut kept = 0u64;
        for r in &rows {
            let (Some(q), Some(c)) = (r.queuename.as_deref(), r.contno.as_deref()) else {
                continue;
            };
            if q.is_empty() || c.is_empty() {
                continue;
            }
            sqlx::query(
                "INSERT INTO live_stow_plan (vessel, voyage, queuename, disload, contno, planseq, as_of_ts)
                 VALUES ($1,$2,$3,$4,$5,$6,$7)
                 ON CONFLICT (vessel, voyage, queuename, contno) DO UPDATE SET
                   disload = EXCLUDED.disload, planseq = EXCLUDED.planseq,
                   as_of_ts = EXCLUDED.as_of_ts",
            )
            .bind(&r.vessel)
            .bind(&r.voyage)
            .bind(q)
            .bind(r.disload.as_deref())
            .bind(c)
            .bind(r.planseq.map(|v| v as i32))
            .bind(as_of)
            .execute(&mut *tx)
            .await
            .context("insert live_stow_plan")?;
            kept += 1;
        }
        tx.commit().await?;

        let dropped = rows.len() as u64 - kept;
        if dropped > 0 {
            tracing::warn!(dropped, "stowplan: 구역이나 상자번호가 없어 버린 행");
        }
        tracing::info!(voyages = used.len(), rows = kept, "stowplan snapshot tick done");
        Ok(kept)
    })
    .await
    .map(|_| ())
}

/// 델타 모드(`STOWPLAN_MODE=delta`, 기본). UPD_DT 워터마크로 바뀐 행만 받아 거울에 병합한다.
async fn src_stowplan_delta(
    pool: &PgPool,
    target: &str,
    date: chrono::NaiveDate,
    as_of: DateTime<Utc>,
) -> Result<()> {
    run_logged(pool, "STOWPLAN_DELTA", date, |_| async move {
        let voyages = active_voyages(pool).await?;
        if voyages.is_empty() {
            tracing::info!("stowplan delta: 작업중인 항차 없음 — Oracle 조회 생략");
            return Ok(0);
        }
        let total = voyages.len();
        let used: Vec<_> = voyages.into_iter().take(MAX_VOYAGES).collect();
        if total > used.len() {
            tracing::warn!(total, used = used.len(), "stowplan delta: 항차가 상한을 넘어 잘랐다");
        }
        let list = voyage_in_list(&used);

        // 워터마크 없음 = 첫 델타 틱. 이번 한 번은 이 항차 범위를 스냅샷처럼 정리하고 돈다.
        let wm: Option<String> =
            sqlx::query_scalar("SELECT max(last_completed_at) FROM etl_watermark WHERE stream = $1")
                .bind(WM_STREAM)
                .fetch_one(pool)
                .await?;
        let first_tick = wm.is_none();
        let wm = wm.unwrap_or_else(|| WM_FLOOR.to_string());

        // 신규 활성 항차 시딩(2026-08-10): 접안으로 막 활성이 된 항차의 계획 행은 UPD_DT 가
        // 과거(계획은 접안 전 작성)라 델타 조건에 절대 안 걸린다 — 종전엔 시간당 recon 까지
        // 최대 1시간 그 배의 순번이 거울에 없었다. 거울에 행이 하나도 없는 활성 항차만 골라
        // 같은 왕복에 UNION ALL 로 통째 읽는다(왕복 증가 0, sql/stowplan_seed.sql 참고).
        // 첫 틱은 워터마크가 바닥이라 델타 자체가 전체를 읽으므로 시딩이 필요 없다.
        // ⚠ 후보는 "시딩됨"이 아니라 "후보"다: 활성 목록에는 계획이 아예 없는 센티넬
        // 유령선박(RHXX 등)과 전 행이 완료된 끝물 항차도 있고, 그쪽은 0행이 돌아온다
        // (실측: RHXX 2건이 상시 후보 — 같은 왕복 안 술어 몇 개라 무해).
        let seed_voyages: Vec<(String, String)> = if first_tick {
            Vec::new()
        } else {
            let present: std::collections::HashSet<(String, String)> =
                sqlx::query_as::<_, (String, String)>(&format!(
                    "SELECT DISTINCT vessel, voyage FROM live_stow_plan WHERE (vessel, voyage) IN ({list})"
                ))
                .fetch_all(pool)
                .await?
                .into_iter()
                .collect();
            used.iter().filter(|v| !present.contains(v)).cloned().collect()
        };

        let mut sql = SQL_STOWPLAN_DELTA.replace("__VOYAGES__", &list).replace("{wm}", &wm);
        if !seed_voyages.is_empty() {
            sql.push_str("\nUNION ALL\n");
            sql.push_str(&SQL_STOWPLAN_SEED.replace("__SEED_VOYAGES__", &voyage_in_list(&seed_voyages)));
        }
        let raw = Toolbox::from_env(target)?.run_sql(&sql).await?;
        let rows: Vec<PlanDeltaRow> = parse_rows(&raw).context("parsing stowplan delta rows")?;

        let mut tx = pool.begin().await?;
        if first_tick {
            // 첫 델타 틱: 이 항차 범위의 거울을 비우고 새로 채운다(옛 스냅샷 행이 다른 큐/구역
            // 이름으로 남아 있으면 델타 UPSERT 만으로는 못 지운다).
            let scoped_del = format!("DELETE FROM live_stow_plan WHERE (vessel, voyage) IN ({list})");
            sqlx::query(&scoped_del).execute(&mut *tx).await?;
        }

        let mut upserted = 0u64;
        let mut deleted = 0u64;
        let mut dropped = 0u64;
        let mut max_upd: Option<String> = None;
        for r in &rows {
            if let Some(u) = r.upd.as_deref() {
                let well_formed = u.len() == 14 && u.bytes().all(|b| b.is_ascii_digit());
                if well_formed && max_upd.as_deref().is_none_or(|m| u > m) {
                    max_upd = Some(u.to_string());
                }
            }
            let Some(c) = r.contno.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
                dropped += 1;
                continue;
            };
            let completed = r
                .compdate
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            if completed {
                // 완료 → 거울에서 지운다. 무결성 키(vessel,voyage,contno,disload) 기준 —
                // 구역(queuename)은 완료 시점엔 이미 의미가 없다.
                sqlx::query(
                    "DELETE FROM live_stow_plan
                      WHERE vessel = $1 AND voyage = $2 AND contno = $3
                        AND disload IS NOT DISTINCT FROM $4",
                )
                .bind(&r.vessel)
                .bind(&r.voyage)
                .bind(c)
                .bind(r.disload.as_deref())
                .execute(&mut *tx)
                .await
                .context("delete completed live_stow_plan")?;
                deleted += 1;
                continue;
            }
            let Some(q) = r.queuename.as_deref().filter(|s| !s.is_empty()) else {
                dropped += 1;
                continue;
            };
            sqlx::query(
                "INSERT INTO live_stow_plan (vessel, voyage, queuename, disload, contno, planseq, as_of_ts)
                 VALUES ($1,$2,$3,$4,$5,$6,$7)
                 ON CONFLICT (vessel, voyage, contno, disload) DO UPDATE SET
                   queuename = EXCLUDED.queuename, planseq = EXCLUDED.planseq,
                   as_of_ts = EXCLUDED.as_of_ts",
            )
            .bind(&r.vessel)
            .bind(&r.voyage)
            .bind(q)
            .bind(r.disload.as_deref())
            .bind(c)
            .bind(r.planseq.map(|v| v as i32))
            .bind(as_of)
            .execute(&mut *tx)
            .await
            .context("upsert live_stow_plan delta")?;
            upserted += 1;
        }

        if let Some(mx) = max_upd {
            // 안전랙만큼 물러선 값을 저장한다(위 doc 참고) — GREATEST 로 역진 방지.
            let seeded = NaiveDateTime::parse_from_str(&mx, "%Y%m%d%H%M%S")
                .map(|t| {
                    (t - chrono::Duration::seconds(SAFETY_LAG_S))
                        .format("%Y%m%d%H%M%S")
                        .to_string()
                })
                .unwrap_or(mx);
            sqlx::query(
                "INSERT INTO etl_watermark (stream, snapshot_date, last_completed_at, updated_at)
                 VALUES ($1, $2, $3, now())
                 ON CONFLICT (stream, snapshot_date) DO UPDATE
                   SET last_completed_at = GREATEST(etl_watermark.last_completed_at, EXCLUDED.last_completed_at),
                       updated_at = now()",
            )
            .bind(WM_STREAM)
            .bind(date)
            .bind(&seeded)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        if dropped > 0 {
            tracing::warn!(dropped, "stowplan delta: 구역이나 상자번호가 없어 버린 행");
        }
        tracing::info!(
            voyages = used.len(),
            rows = rows.len(),
            upserted,
            deleted,
            first_tick,
            seed_candidates = seed_voyages.len(),
            "stowplan delta tick done"
        );
        Ok(rows.len() as u64)
    })
    .await
    .map(|_| ())
}

/// 화해(reconcile): 전체 스냅샷을 받아 거울과 diff, 불일치만 로그하고 스냅샷으로 교체한다.
/// `extractor stowplan --reconcile` — `tt-stowplan-recon.timer`(1시간)에서 호출.
pub async fn reconcile_stowplan(pool: &PgPool, target: &str) -> Result<()> {
    let date = tt_core::shift::terminal_now().date_naive();
    run_logged(pool, "STOWPLAN_RECON", date, |_| async move {
        let voyages = active_voyages(pool).await?;
        if voyages.is_empty() {
            tracing::info!("stowplan recon: 작업중인 항차 없음 — Oracle 조회 생략");
            return Ok(0);
        }
        let total = voyages.len();
        let used: Vec<_> = voyages.into_iter().take(MAX_VOYAGES).collect();
        if total > used.len() {
            tracing::warn!(total, used = used.len(), "stowplan recon: 항차가 상한을 넘어 잘랐다");
        }
        let list = voyage_in_list(&used);
        let sql = SQL_STOWPLAN.replace("__VOYAGES__", &list);

        let raw = Toolbox::from_env(target)?.run_sql(&sql).await?;
        let truth: Vec<PlanRow> = parse_rows(&raw).context("parsing stowplan recon rows")?;

        // 지금 거울에 있는, 같은 항차 범위의 행(무결성 키 기준).
        let mirror: Vec<(String, String, String, Option<String>, Option<i32>)> = sqlx::query_as(&format!(
            "SELECT vessel, voyage, contno, disload, planseq FROM live_stow_plan
              WHERE (vessel, voyage) IN ({list})"
        ))
        .fetch_all(pool)
        .await?;

        use std::collections::HashMap;
        let mut truth_map: HashMap<(String, String, String, Option<String>), Option<i32>> = HashMap::new();
        for r in &truth {
            let (Some(q), Some(c)) = (r.queuename.as_deref(), r.contno.as_deref()) else {
                continue;
            };
            if q.is_empty() || c.is_empty() {
                continue;
            }
            truth_map.insert(
                (r.vessel.clone(), r.voyage.clone(), c.to_string(), r.disload.clone()),
                r.planseq.map(|v| v as i32),
            );
        }
        let mut mirror_set: std::collections::HashSet<(String, String, String, Option<String>)> =
            std::collections::HashSet::new();
        let mut drift = 0u64;
        for (v, y, c, d, planseq) in &mirror {
            let key = (v.clone(), y.clone(), c.clone(), d.clone());
            mirror_set.insert(key.clone());
            match truth_map.get(&key) {
                None => drift += 1,           // 거울에 있는데 Oracle 진실엔 없다(완료/제외됨)
                Some(t) if t != planseq => drift += 1, // 값이 다르다
                _ => {}
            }
        }
        for key in truth_map.keys() {
            if !mirror_set.contains(key) {
                drift += 1; // Oracle 진실에 있는데 거울에 없다(놓친 델타)
            }
        }

        // 스냅샷으로 교체(기존 전체교체 코드와 동일 — 한 트랜잭션).
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM live_stow_plan").execute(&mut *tx).await?;
        let as_of = Utc::now();
        let mut kept = 0u64;
        for r in &truth {
            let (Some(q), Some(c)) = (r.queuename.as_deref(), r.contno.as_deref()) else {
                continue;
            };
            if q.is_empty() || c.is_empty() {
                continue;
            }
            sqlx::query(
                "INSERT INTO live_stow_plan (vessel, voyage, queuename, disload, contno, planseq, as_of_ts)
                 VALUES ($1,$2,$3,$4,$5,$6,$7)
                 ON CONFLICT (vessel, voyage, queuename, contno) DO UPDATE SET
                   disload = EXCLUDED.disload, planseq = EXCLUDED.planseq,
                   as_of_ts = EXCLUDED.as_of_ts",
            )
            .bind(&r.vessel)
            .bind(&r.voyage)
            .bind(q)
            .bind(r.disload.as_deref())
            .bind(c)
            .bind(r.planseq.map(|v| v as i32))
            .bind(as_of)
            .execute(&mut *tx)
            .await
            .context("insert live_stow_plan (recon)")?;
            kept += 1;
        }
        tx.commit().await?;

        tracing::info!(voyages = used.len(), rows = kept, drift, fixed = drift, "recon drift={drift} fixed={drift}");
        Ok(kept)
    })
    .await
    .map(|_| ())
}
