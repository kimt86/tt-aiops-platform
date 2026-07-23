-- 0101_retire_dead_extracts.sql
-- Retire two confirmed-dead artifacts (2026-07-22 audit; no readers anywhere in
-- crates/, web/src/, scripts/, or SQL):
--
--   * raw_k_mph_voyage  — K_MPH voyage-level extract (created in 0002). The nightly
--                         `run --kpi all` kept populating it with a 30-day-window
--                         Oracle query, but nothing ever read the table. The
--                         extractor module (kpis/k_mph_voyage.rs) + its c06 SQL are
--                         removed in the same change, so it is now unreachable.
--
--   * kpi_breakdown_qc  — L1 table (created in 0004, altered in 0006) written by
--                         transform.rs::breakdown_qc(). Never SELECTed: the live
--                         /api/breakdown/qc endpoint recomputes per-QC figures from
--                         raw_k_mph_realtime + vessel_qc_shift + raw_k_qc_q directly.
--                         The transform writer is removed in the same change.
--                         NOTE: only the TABLE is dead — the endpoint stays.
DROP TABLE IF EXISTS raw_k_mph_voyage;
DROP TABLE IF EXISTS kpi_breakdown_qc;
