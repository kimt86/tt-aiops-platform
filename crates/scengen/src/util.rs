//! Shared pure helpers (MYT time, block/cell/ISO decoding) used across collect/snapshot/enrich.

use chrono::{DateTime, FixedOffset, NaiveDateTime, Utc};

/// MYT "YYYYMMDDHHMMSS[...]" → UTC. Terminal is UTC+8; we store canonical UTC.
pub fn parse_myt(s: &str) -> Option<DateTime<Utc>> {
    let base = s.trim().get(..14)?;
    let naive = NaiveDateTime::parse_from_str(base, "%Y%m%d%H%M%S").ok()?;
    let myt = FixedOffset::east_opt(8 * 3600)?;
    Some(naive.and_local_timezone(myt).single()?.with_timezone(&Utc))
}

/// yard block = first token of YT_TOPOS ("04U-0809" → "04U").
pub fn parse_block(topos: &str) -> Option<String> {
    let b = topos.trim().split('-').next().unwrap_or("").trim();
    (!b.is_empty()).then(|| b.to_string())
}

/// ISO 6346 size/type → (size, height, family). Family may be overridden by CONTTYPE upstream.
pub fn decode_iso(iso: &str) -> (&'static str, &'static str, &'static str) {
    let b = iso.trim().as_bytes();
    let size = match b.first() {
        Some(b'2') => "twenty",
        Some(b'4') => "forty",
        Some(b'L') | Some(b'M') | Some(b'N') => "forty_five",
        _ => "forty",
    };
    let height = match b.get(1) {
        Some(b'5') | Some(b'6') | Some(b'L') | Some(b'M') | Some(b'N') => "high_cube",
        _ => "standard",
    };
    let family = match b.get(2) {
        Some(b'R') => "reefer",
        Some(b'T') => "tank",
        Some(b'U') => "open_top",
        Some(b'P') => "flat_rack",
        _ => "general",
    };
    (size, height, family)
}

/// Stowage code → (bay, row, tier). Row/tier are always the last 2+2 digits; bay is what precedes,
/// so this handles both 6-char BBRRTT (2-digit bay, this terminal) and 7-char BBBRRTT (ISO 9711).
pub fn parse_cell(s: &str) -> (Option<i32>, Option<i32>, Option<i32>) {
    let t = s.trim();
    let n = t.len();
    if n < 5 || !t.bytes().all(|b| b.is_ascii_digit()) {
        return (None, None, None);
    }
    let bay = t.get(0..n - 4).and_then(|x| x.parse().ok());
    let row = t.get(n - 4..n - 2).and_then(|x| x.parse().ok());
    let tier = t.get(n - 2..n).and_then(|x| x.parse().ok());
    (bay, row, tier)
}

/// Parse a numeric string (weight etc.) to i32 kg, tolerating decimals/blanks.
pub fn parse_num(s: Option<&str>) -> Option<i32> {
    let t = s?.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok().map(|v| v.round() as i32)
}

/// True iff `s` is a well-formed watermark key: all digits and at least 14 of them
/// ("YYYYMMDDHHMMSS", optionally with a millisecond tail). Watermarks may ONLY advance on these —
/// a malformed key sorts lexicographically out of order and could either jump the watermark far
/// ahead (silently skipping rows) or stall it. Refusing to advance is the safe failure direction.
pub fn is_wm_key(s: &str) -> bool {
    s.len() >= 14 && s.bytes().all(|b| b.is_ascii_digit())
}

/// Watermark minus a safety lag, as a 14-digit "YYYYMMDDHHMMSS" seek bound.
///
/// TOS makes rows visible slightly out of key order (measured ~1 in 866k, up to 3s), so seeking
/// from exactly the stored watermark can step over a row that only became visible after we had
/// already read past its key. Seeking from (wm − lag) re-reads the recent tail so ON CONFLICT can
/// dedup it, which makes skipping provably zero. A 14-digit bound is also a correct lower bound
/// for longer keys, since a 17-digit key sharing a later 14-prefix still sorts after it.
pub fn wm_minus_secs(s: &str, secs: i64) -> Option<String> {
    if !is_wm_key(s) {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(s.get(..14)?, "%Y%m%d%H%M%S").ok()?;
    Some((naive - chrono::Duration::seconds(secs)).format("%Y%m%d%H%M%S").to_string())
}

/// Row index (0-based) -> row letter ("A".."Z"); falls back to the number beyond Z.
pub fn row_name(idx: i32) -> String {
    if (0..=25).contains(&idx) {
        ((b'A' + idx as u8) as char).to_string()
    } else {
        idx.to_string()
    }
}

/// Extract a JSON field as Option<String>, accepting either a string or a number (the
/// remote-toolbox serializer emits numeric-looking values as JSON numbers).
pub fn jstr(row: &serde_json::Value, key: &str) -> Option<String> {
    match row.get(key) {
        Some(serde_json::Value::String(s)) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}
