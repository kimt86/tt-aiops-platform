-- Store the raw handover coordinates alongside the (gridded) zone, so the quay GPS-grid can be
-- RE-bucketed retroactively if we later tighten it (e.g. 150m → 75m). Without these, a future
-- grid change only applies forward and strands already-accumulated quay samples at the old size.
-- Added now (quay data ≈ 0) so the whole history stays re-griddable. Block endpoints carry coords
-- too (free), though block zones use logical codes. NULL where the leg had no captured coord.
ALTER TABLE learn_travel_sample
  ADD COLUMN IF NOT EXISTS origin_lat DOUBLE PRECISION,
  ADD COLUMN IF NOT EXISTS origin_lon DOUBLE PRECISION,
  ADD COLUMN IF NOT EXISTS dest_lat   DOUBLE PRECISION,
  ADD COLUMN IF NOT EXISTS dest_lon   DOUBLE PRECISION;
