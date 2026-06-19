//! Canonical KPI identifiers and their display metadata.

use serde::{Deserialize, Serialize};

/// The six headline KPIs the dashboard serves (Phase-E research definitions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KpiKey {
    KUtil,
    KEmpty,
    KEmptyR,
    KCycle,
    KRtgQ,
    KMph,
    KQcNomove,
    KQcTtWait,
    KQcTtWaitGps,
}

impl KpiKey {
    pub const ALL: [KpiKey; 9] = [
        KpiKey::KUtil,
        KpiKey::KEmpty,
        KpiKey::KEmptyR,
        KpiKey::KCycle,
        KpiKey::KRtgQ,
        KpiKey::KMph,
        KpiKey::KQcNomove,
        KpiKey::KQcTtWait,
        KpiKey::KQcTtWaitGps,
    ];

    /// The string key used in the database (`kpi_daily.kpi_key`) and API.
    pub fn as_str(&self) -> &'static str {
        match self {
            KpiKey::KUtil => "K_UTIL",
            KpiKey::KEmpty => "K_EMPTY",
            KpiKey::KEmptyR => "K_EMPTY_R",
            KpiKey::KCycle => "K_CYCLE",
            KpiKey::KRtgQ => "K_RTG_Q",
            KpiKey::KMph => "K_MPH",
            KpiKey::KQcNomove => "K_QC_NOMOVE",
            KpiKey::KQcTtWait => "K_QC_TT_WAIT",
            KpiKey::KQcTtWaitGps => "K_QC_TT_WAIT_GPS",
        }
    }

    /// Human-readable English name (never expose the `K_*` key in the UI).
    pub fn name_en(&self) -> &'static str {
        match self {
            KpiKey::KUtil => "TT Utilization",
            KpiKey::KEmpty => "Empty Travel / Job",
            KpiKey::KEmptyR => "Empty Travel Ratio",
            KpiKey::KCycle => "TT Cycle Time",
            KpiKey::KRtgQ => "RTG Handover Wait",
            KpiKey::KMph => "QC Moves / Hour",
            KpiKey::KQcNomove => "QC No-Move Idle",
            KpiKey::KQcTtWait => "QC Wait for Truck",
            KpiKey::KQcTtWaitGps => "QC Truck-Wait (GPS)",
        }
    }

    /// Human-readable Korean name.
    pub fn name_ko(&self) -> &'static str {
        match self {
            KpiKey::KUtil => "TT 가동률",
            KpiKey::KEmpty => "공차 이동거리/작업",
            KpiKey::KEmptyR => "공차 이동 비율",
            KpiKey::KCycle => "TT 사이클 타임",
            KpiKey::KRtgQ => "야드(RTG) 핸드오버 대기",
            KpiKey::KMph => "QC 시간당 처리량",
            KpiKey::KQcNomove => "QC 무브 공백",
            KpiKey::KQcTtWait => "QC 트럭 대기",
            KpiKey::KQcTtWaitGps => "QC 트럭대기(GPS)",
        }
    }

    /// Display unit for the headline value.
    pub fn unit(&self) -> &'static str {
        match self {
            KpiKey::KUtil => "%",
            KpiKey::KEmpty => "km/Job",
            KpiKey::KEmptyR => "%",
            KpiKey::KCycle => "s",
            KpiKey::KRtgQ => "s",
            KpiKey::KMph => "move/hr",
            KpiKey::KQcNomove => "s",
            KpiKey::KQcTtWait => "s",
            KpiKey::KQcTtWaitGps => "QC",
        }
    }

    /// True if higher values are better (drives delta colour in the UI).
    pub fn higher_is_better(&self) -> bool {
        matches!(self, KpiKey::KUtil | KpiKey::KMph)
    }

    pub fn from_str(s: &str) -> Option<KpiKey> {
        KpiKey::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_str() {
        for k in KpiKey::ALL {
            assert_eq!(KpiKey::from_str(k.as_str()), Some(k));
        }
        assert_eq!(KpiKey::from_str("NOPE"), None);
    }
}
