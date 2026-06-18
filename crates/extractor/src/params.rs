//! Date / time-window parameterization of the validated SQL.
//!
//! Two kinds of windowed SQL:
//! - **Type A** (MCH_OPERATION / JOB_ORDER_HISTORY KPIs): carry `{{DAY_STR}}` and a
//!   `{{TIME_PREDICATE}}` token placed right after the date predicate. DAY path fills
//!   it with `""` (so the SQL is byte-identical to the validated version); SHIFT path
//!   fills it with `AND (<time-col>) BETWEEN '<start>' AND '<end>'`.
//! - **Type B** (K_UTIL, MCH_WORKTIME): its params CTE uses `{{START_TS}}`/`{{END_TS}}`
//!   for the clip window and `{{ELAPSED_DENOM}}` for the utilisation denominator.
//!   DAY path = full-day bounds + 1440.0; SHIFT path = shift window + elapsed minutes.
//!
//! No Oracle date math, no bind variables — index-safe string literals only. After
//! substitution we assert no `{{...}}` remains so a missing placeholder fails loudly.

use anyhow::{bail, Result};
use chrono::{NaiveDate, NaiveDateTime};

pub const DAY_STR_TOKEN: &str = "{{DAY_STR}}";
pub const START_TS_TOKEN: &str = "{{START_TS}}";
pub const END_TS_TOKEN: &str = "{{END_TS}}";
pub const ELAPSED_DENOM_TOKEN: &str = "{{ELAPSED_DENOM}}";
pub const TIME_PREDICATE_TOKEN: &str = "{{TIME_PREDICATE}}";
// K_QC_NOMOVE min idle-gaps-per-QC threshold: day=10 (reliable), shift=2 (so a partial
// shift still yields an approximate value instead of nothing).
pub const QCQ_HAVING_TOKEN: &str = "{{QCQ_HAVING}}";

/// Which concatenated date+time column a Type-A shift predicate filters on. The
/// expressions yield a 14-char YYYYMMDDHH24MISS string comparable to START/END_TS.
#[derive(Clone, Copy)]
pub enum TimeCol {
    /// MCH_OPERATION: COMPDATE(8) || COMPTIME(6) = 14 chars.
    MchOper,
    /// JOB_ORDER_HISTORY: JOB_HIST_DATE(8) || SUBSTR(JOB_HIST_TIME,1,6) = 14 chars.
    JobHist,
}

impl TimeCol {
    fn expr(&self) -> &'static str {
        match self {
            TimeCol::MchOper => "MCH_OPER_COMPDATE||MCH_OPER_COMPTIME",
            TimeCol::JobHist => "JOB_HIST_DATE||SUBSTR(JOB_HIST_TIME,1,6)",
        }
    }
}

pub fn day_str(date: NaiveDate) -> String {
    date.format("%Y%m%d").to_string()
}

/// Replace each (token, value) where present, then assert no `{{` token remains.
pub fn render(template: &str, subs: &[(&str, String)]) -> Result<String> {
    let mut out = template.to_string();
    for (token, value) in subs {
        if out.contains(token) {
            out = out.replace(token, value);
        }
    }
    if let Some(idx) = out.find("{{") {
        let tail = &out[idx..out.len().min(idx + 40)];
        bail!("SQL still has an unfilled placeholder near: {tail}");
    }
    Ok(out)
}

/// DAY path: empty time predicate (byte-identical Type-A SQL) + full-day Type-B bounds.
pub fn render_day(template: &str, date: NaiveDate) -> Result<String> {
    let d = day_str(date);
    render(
        template,
        &[
            (DAY_STR_TOKEN, d.clone()),
            (TIME_PREDICATE_TOKEN, String::new()),
            (START_TS_TOKEN, format!("{d}000000")),
            (END_TS_TOKEN, format!("{d}235959")),
            (ELAPSED_DENOM_TOKEN, "1440.0".to_string()),
            (QCQ_HAVING_TOKEN, "10".to_string()),
        ],
    )
}

/// SHIFT path. `time_col` = the Type-A predicate column (None for Type-B K_UTIL).
/// `business_date` is the shift's calendar day; window = `[start, min(end, day-end)]`.
pub fn render_shift(
    template: &str,
    business_date: NaiveDate,
    start: NaiveDateTime,
    end: NaiveDateTime,
    time_col: Option<TimeCol>,
) -> Result<String> {
    let d = day_str(business_date);
    let day_end = business_date.and_hms_opt(23, 59, 59).unwrap();
    let end_capped = end.min(day_end).max(start);
    let start_ts = start.format("%Y%m%d%H%M%S").to_string();
    let end_ts = end_capped.format("%Y%m%d%H%M%S").to_string();
    let elapsed_min = ((end_capped - start).num_seconds() as f64 / 60.0).max(1.0);
    let time_predicate = match time_col {
        Some(c) => format!("AND ({}) BETWEEN '{}' AND '{}'", c.expr(), start_ts, end_ts),
        None => String::new(),
    };
    render(
        template,
        &[
            (DAY_STR_TOKEN, d),
            (TIME_PREDICATE_TOKEN, time_predicate),
            (START_TS_TOKEN, start_ts),
            (END_TS_TOKEN, end_ts),
            (ELAPSED_DENOM_TOKEN, format!("{elapsed_min:.4}")),
            (QCQ_HAVING_TOKEN, "2".to_string()),
        ],
    )
}

/// Voyage-window template: `{{START_TS}}` = (date - days_back) at 00:00:00.
pub fn render_window(template: &str, date: NaiveDate, days_back: i64) -> Result<String> {
    let start = date - chrono::Duration::days(days_back);
    let start_ts = start.format("%Y%m%d000000").to_string();
    render(template, &[(START_TS_TOKEN, start_ts)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_path_typea_byte_identical() {
        // a Type-A template: the TIME_PREDICATE line collapses to empty on the day path
        let t = "WHERE MCH_OPER_COMPDATE = '{{DAY_STR}}'\n   {{TIME_PREDICATE}}\n AND x";
        let out = render_day(t, NaiveDate::from_ymd_opt(2026, 6, 4).unwrap()).unwrap();
        assert_eq!(out, "WHERE MCH_OPER_COMPDATE = '20260604'\n   \n AND x");
    }

    #[test]
    fn shift_path_typea_clause() {
        let t = "WHERE MCH_OPER_COMPDATE = '{{DAY_STR}}'\n   {{TIME_PREDICATE}}";
        let d = NaiveDate::from_ymd_opt(2026, 6, 5).unwrap();
        let out = render_shift(t, d, d.and_hms_opt(8, 0, 0).unwrap(), d.and_hms_opt(10, 0, 0).unwrap(), Some(TimeCol::MchOper)).unwrap();
        assert!(out.contains("AND (MCH_OPER_COMPDATE||MCH_OPER_COMPTIME) BETWEEN '20260605080000' AND '20260605100000'"), "{out}");
    }

    #[test]
    fn shift_path_typeb_util_denom() {
        let t = "x BETWEEN '{{START_TS}}' AND '{{END_TS}}' / {{ELAPSED_DENOM}}";
        let d = NaiveDate::from_ymd_opt(2026, 6, 5).unwrap();
        let out = render_shift(t, d, d.and_hms_opt(8, 0, 0).unwrap(), d.and_hms_opt(10, 0, 0).unwrap(), None).unwrap();
        assert!(out.contains("'20260605080000' AND '20260605100000' / 120.0000"), "{out}");
    }

    #[test]
    fn leftover_token_errors() {
        assert!(render("a {{DAY_STR}} {{NOPE}}", &[(DAY_STR_TOKEN, "x".into())]).is_err());
    }
}
