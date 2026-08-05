//! 적부계획의 상자별 작업 순번(TOS `VSP_SHIP.VSP_SHP_PLANSEQ`) → `live_stow_plan`.
//!
//! **왜 있나.** 구역 안에서 "이 상자가 몇 번째인가"를 우리는 작업지시 생성시각으로 추정하고
//! 있었는데, 작업지시 표에는 순서가 없다 — `MSNSEQ` 는 전부 비어 있고, `SEQNO` 는 발행 시각이며
//! 완료되면 완료 시각으로 덮어쓰인다(끝난 작업으로 채점하면 100% 로 보이는 함정). 그래서 상자
//! 79.5% 가 다른 상자와 같은 값을 공유했고 순번이 사실상 임의였다.
//!
//! **TOS 자신은** ITV 배차기에서 이렇게 정렬한다 —
//! `ORDER BY JOB_QUE_PLND_DATE||TIME, VSP_SHP_PLANSEQ` (LoadableJob.xml). 순서는 작업지시가
//! 아니라 적부계획에 있다.
//!
//! **알갱이(실측 2026-08-05).** 구역당 상자 중앙 35개 · 구역 안 서로 다른 순번값 중앙 35개
//! ⇒ 값 하나당 1.0 상자. 진짜 상자 단위 순번이다.
//!
//! ⚠ **적하 전용.** 양하는 원천이 13.6% 만 채워져 있어 쓸 수 없다.
//! ⚠ **부하는 질의의 세 조건이 전부다**(sql/stowplan.sql 참고). 좁히면 2.3초/4,725행,
//!   적하 조건 하나만 빼도 16.4초/22,233행이 된다.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::runner::Toolbox;

const SQL_STOWPLAN: &str = include_str!("../sql/stowplan.sql");

/// 한 주기에 조회할 항차 수 상한. 넘으면 자르고 경고한다 — 조용히 줄이면 "다 봤다"로 읽힌다.
/// 실측: 항차 26개·미완료 적하 4,725행에 2.3초. 40이면 배 이상 여유다.
const MAX_VOYAGES: usize = 40;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct PlanRow {
    pub vessel: String,
    pub voyage: String,
    pub queuename: Option<String>,
    pub contno: Option<String>,
    /// NUMBER 라 툴박스가 JSON 숫자로 준다. `Option<String>` 으로 받으면 배치째 디코드가 깨진다.
    pub planseq: Option<i64>,
}

pub async fn tick_stowplan(pool: &PgPool, target: &str) -> Result<()> {
    let date = tt_core::shift::terminal_now().date_naive();
    let as_of = Utc::now();
    src_stowplan(pool, target, date, as_of).await
}

async fn src_stowplan(
    pool: &PgPool,
    target: &str,
    date: chrono::NaiveDate,
    as_of: DateTime<Utc>,
) -> Result<()> {
    run_logged(pool, "STOWPLAN", date, |_| async move {
        // 조회 범위는 **우리 Postgres 에서** 만든다 — Oracle 부하 0. 지금 크레인이 붙어 실제로
        // 작업이 진행중인 항차만. (붙기만 하고 아직 시작 안 한 배까지 넣으면 항차가 두 배가 된다)
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

        if voyages.is_empty() {
            tracing::info!("stowplan: 작업중인 항차 없음 — Oracle 조회 생략");
            return Ok(0);
        }
        let total = voyages.len();
        let used: Vec<_> = voyages.into_iter().take(MAX_VOYAGES).collect();
        if total > used.len() {
            tracing::warn!(total, used = used.len(), "stowplan: 항차가 상한을 넘어 잘랐다");
        }

        // 작은따옴표는 SQL 리터럴이 되므로 두 번 써서 무력화한다. 선박/항차는 TOS 코드값이라
        // 원래 따옴표가 들어올 일이 없지만, 들어와도 질의가 깨지지 않게 해 둔다.
        let esc = |s: &str| s.replace('\'', "''");
        let list = used
            .iter()
            .map(|(v, y)| format!("('{}','{}')", esc(v), esc(y)))
            .collect::<Vec<_>>()
            .join(",");
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
                "INSERT INTO live_stow_plan (vessel, voyage, queuename, contno, planseq, as_of_ts)
                 VALUES ($1,$2,$3,$4,$5,$6)
                 ON CONFLICT (vessel, voyage, queuename, contno) DO UPDATE SET
                   planseq = EXCLUDED.planseq, as_of_ts = EXCLUDED.as_of_ts",
            )
            .bind(&r.vessel)
            .bind(&r.voyage)
            .bind(q)
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
        tracing::info!(voyages = used.len(), rows = kept, "stowplan tick done");
        Ok(kept)
    })
    .await
    .map(|_| ())
}
