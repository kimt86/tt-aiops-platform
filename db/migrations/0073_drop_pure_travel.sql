-- Retire the PURE-driving travel-time model (②). It fed the Stage-2 dispatch cost as moving-only
-- (stop-excluded) time, but that under-counts real arrival in a ROUTE-DEPENDENT way (realized/pure
-- ratio ≈ 9.9× for short legs → 2.3× for long legs), so it compressed near-vs-far truck spread and
-- mis-ranked min-cost picks — with no congestion multiplier ever wired in. The cost now uses the
-- REALIZED zone summary (learn_travel_zone225), which carries route congestion empirically (lower
-- bias, ~3× broader coverage). The pure pipeline had no other live consumer (zone225_pure only the
-- cost; topos_pure refreshed into the void; learn_eval was a circular pure-vs-pure self-score).
-- Road-network GRAPH inference (truck_pos_hifreq → scripts/cron → livemap layer) is separate and stays.
DROP MATERIALIZED VIEW IF EXISTS learn_travel_zone225_pure;
DROP MATERIALIZED VIEW IF EXISTS learn_travel_topos_pure;
DROP TABLE IF EXISTS learn_travel_drive_sample;
DROP TABLE IF EXISTS learn_travel_topos_sample;
DROP TABLE IF EXISTS learn_eval;
