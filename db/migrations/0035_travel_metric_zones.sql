-- Track the zone-model quality over time (the overnight accumulation curve): how many zone OD
-- pairs exist, how many are "confident" (n≥10), and how many samples carry a quay GPS-grid zone
-- (which only accrues forward from the Phase-2 coordinate capture). Lets us watch confident zone
-- pairs recover as quay-grid samples accumulate. See research/travel-time.
ALTER TABLE learn_travel_metric
  ADD COLUMN IF NOT EXISTS zone_pairs           INT,
  ADD COLUMN IF NOT EXISTS confident_zone_pairs INT,
  ADD COLUMN IF NOT EXISTS quay_zoned_samples   INT;
