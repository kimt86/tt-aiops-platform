-- Wire the empty-leg decomposition into dispatch (verified: approach-stop varies ~9× across cranes
-- AND ~2.5× when a truck queues, so it is a DESTINATION property, not a per-truck one):
--   (1) Stage-2 cost = PURE DRIVE time (moving-only), keyed by 225m OD grid.
--   (2) the approach/handover stop split out as a per-crane Stage-1 signal.
-- learn_leg_decomp gains the 225m grid keys so its drive_s can be aggregated into the cost layer.
ALTER TABLE learn_leg_decomp ADD COLUMN IF NOT EXISTS oz text;  -- origin 225m grid (first GPS fix)
ALTER TABLE learn_leg_decomp ADD COLUMN IF NOT EXISTS dz text;  -- dest 225m grid (pickup coord)

-- Stage-2 cost layer: PURE-drive OD = moving-only time per origin→dest grid cell. Replaces the realized
-- zone225 (which bundled the dest-side approach/handover into per-truck time → noisier, and it cancels
-- in same-destination truck ranking anyway). Built from learn_leg_decomp.drive_s, refreshed every 5min.
DROP MATERIALIZED VIEW IF EXISTS learn_travel_zone225_drive;
CREATE MATERIALIZED VIEW learn_travel_zone225_drive AS
  SELECT oz, dz, count(*) AS n,
         percentile_cont(0.5) WITHIN GROUP (ORDER BY drive_s)::int AS p50_s,
         percentile_cont(0.9) WITHIN GROUP (ORDER BY drive_s)::int AS p90_s
    FROM learn_leg_decomp
   WHERE oz IS NOT NULL AND dz IS NOT NULL AND drive_s > 0
   GROUP BY oz, dz;
CREATE UNIQUE INDEX learn_travel_zone225_drive_pk ON learn_travel_zone225_drive (oz, dz);

-- Stage-1 signal: per-crane (pickup) APPROACH/handover overhead = time from GPS physical arrival to TOS
-- ARRIVED. Destination-property (not which-truck), so it belongs in Stage-1 timing, not the per-truck
-- Stage-2 cost. med = static handover overhead per crane; (dynamic queue depth is a later live signal).
DROP MATERIALIZED VIEW IF EXISTS learn_crane_approach;
CREATE MATERIALIZED VIEW learn_crane_approach AS
  SELECT dest_topos,
         count(*) AS n,
         percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM arrived_at - gps_arrived_at))::int AS med_approach_s,
         percentile_cont(0.9) WITHIN GROUP (ORDER BY extract(epoch FROM arrived_at - gps_arrived_at))::int AS p90_approach_s
    FROM learn_leg_decomp
   WHERE gps_arrived_at IS NOT NULL
     AND extract(epoch FROM arrived_at - gps_arrived_at) BETWEEN 0 AND 1800
   GROUP BY dest_topos;
CREATE UNIQUE INDEX learn_crane_approach_pk ON learn_crane_approach (dest_topos);
