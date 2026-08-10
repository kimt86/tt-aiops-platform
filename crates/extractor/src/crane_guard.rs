//! QC 크레인 이름 드리프트 가드 (nightly).
//!
//! 배경(2026-08-10~11): "QC = C##"이라는 사전조사 추정이 6주간 살아남아 M·Z 계열(안벽
//! 무브의 ~26%)이 KPI에서 빠졌고, [CMZ]로 넓힌 다음 날 CR4(HLLO 항차 전담)가 또 빠진 것이
//! 드러났다. 접두사 하드코딩은 어떤 목록이든 언젠가 낡는다. 그래서:
//!   · 수집기(qc_moves.rs)는 장비 마스터(CDY_MACHINE type=QC) 조인으로 바꿔 자동 추종하고,
//!   · KPI SQL 14곳의 정규식은 성능·가독성 때문에 남기되(현재 `^(C|CR|DC|M|Z)[0-9]+$`),
//!   · 이 가드가 밤마다 마스터와 정규식을 대조해 어긋나는 즉시 ops_alert 로 알린다.
//! 마스터는 78행짜리 미니 테이블이라 부하는 0에 가깝다.

use anyhow::{Context, Result};
use serde::Deserialize;
use sqlx::PgPool;

use crate::runner::Toolbox;

/// KPI SQL 14곳(c07·c10·e1c·f2·qc_move_time + local 9본)과 반드시 같은 패턴이어야 한다.
/// 이 상수를 바꾸면 그 파일들도 같이 바꾸고, KC kpi-computation.html 의 단차 고지도 갱신할 것.
pub const KPI_QC_PATTERN: &str = "^(C|CR|DC|M|Z)[0-9]+$";

#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct CodeRow {
    code: Option<String>,
}

/// 장비 마스터의 QC 코드 전량을 읽어 KPI 정규식과 대조한다. 어긋난 코드가 있으면
/// ops_alert(crit) — 그 크레인의 무브는 수집기(마스터 조인)에는 들어오지만 KPI 에서는
/// 조용히 빠지고 있다는 뜻이다.
pub async fn qc_master_guard(pool: &PgPool, target: &str) -> Result<()> {
    let raw = Toolbox::from_env(target)?
        .run_sql("SELECT CDY_MCHN_CODE AS code FROM TOSADM.CDY_MACHINE WHERE CDY_MCHN_TYPE = 'QC'")
        .await
        .context("fetching QC codes from CDY_MACHINE")?;
    let rows: Vec<CodeRow> = tt_core::parse::parse_rows(&raw)?;
    let codes: Vec<String> = rows
        .into_iter()
        .filter_map(|r| r.code.map(|c| c.trim().to_string()))
        .filter(|c| !c.is_empty())
        .collect();
    anyhow::ensure!(!codes.is_empty(), "QC master returned no codes — refusing to judge");

    // 정규식 대조는 Postgres 에 맡긴다(새 crate 의존 없이 같은 ~ 시맨틱).
    let bad: Vec<(String,)> = sqlx::query_as(
        "SELECT c FROM unnest($1::text[]) AS c WHERE c !~ $2 ORDER BY c",
    )
    .bind(&codes)
    .bind(KPI_QC_PATTERN)
    .fetch_all(pool)
    .await?;

    if bad.is_empty() {
        tracing::info!(n_qc = codes.len(), pattern = KPI_QC_PATTERN, "qc master guard OK");
        return Ok(());
    }
    let names: Vec<&str> = bad.iter().map(|(c,)| c.as_str()).collect();
    let msg = format!(
        "장비 마스터의 QC {}대가 KPI 크레인 정규식({})에 안 잡힌다 — 그 무브는 수집은 되지만 KPI에서 빠진다. KPI SQL 14곳 + crane_guard::KPI_QC_PATTERN 확장 필요",
        names.len(),
        KPI_QC_PATTERN,
    );
    tracing::error!(missing = ?names, "qc master guard: pattern drift");
    sqlx::query(
        "INSERT INTO ops_alert (source, subject, severity, message, detail)
              VALUES ('extractor', 'qc_master_drift', 'crit', $1, $2)
         ON CONFLICT (source, subject) DO UPDATE
            SET last_ts = now(), occurrences = ops_alert.occurrences + 1,
                severity = EXCLUDED.severity, message = EXCLUDED.message, detail = EXCLUDED.detail",
    )
    .bind(&msg)
    .bind(names.join(","))
    .execute(pool)
    .await?;
    Ok(())
}
