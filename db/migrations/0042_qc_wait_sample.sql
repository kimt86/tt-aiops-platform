-- K_QC_TT_WAIT_GPS history: periodic snapshots of live QC starvation (QC PLC idle + no truck).
-- Logs TWO definitions side-by-side so we can judge reliability:
--   *_topos = current live logic (no TT whose topos1 destination code = this crane). Cheap but
--             vulnerable to the event-driven feed dropping topos1 (truck present yet looks absent).
--   *_gps   = distance-based (no fresh TT within ~40m of the crane position) — robust to topos1
--             staleness; the truer "no truck physically under the crane".
-- Forward-only (GPS history is memory-only). Pruned to 14 days.
CREATE TABLE IF NOT EXISTS qc_wait_sample (
  ts              timestamptz NOT NULL PRIMARY KEY,
  working_qc      int NOT NULL,           -- quay cranes working (move within 1h) + fresh PLC
  starving_topos  int NOT NULL,           -- of those, idle >2min AND no topos1-assigned truck
  wait_topos_s    int,                    -- avg idle of the topos-starving cranes (NULL if 0)
  starving_gps    int NOT NULL,           -- idle >2min AND no TT within ~40m (GPS distance)
  wait_gps_s      int,                    -- avg idle of the gps-starving cranes (NULL if 0)
  starving_both   int,                    -- idle cranes BOTH defs call starving (high-confidence)
  pos_known_qc    int NOT NULL DEFAULT 0  -- working cranes with a known position (scored denominator)
);
-- topos and gps are scored over the SAME population (working cranes with a known position), so
-- starving_topos vs starving_gps is apples-to-apples; topos_only (= starving_topos - starving_both)
-- ≈ false alarms from dropped topos1 fields (a truck IS near per GPS but topos1 did not say so).
