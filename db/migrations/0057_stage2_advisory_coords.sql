-- Stage B (advisory display): log the recommendation's endpoints so the live map can draw a
-- truck→work line. src = the truck's position at recommendation time; dest = the work pickup point.
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS dest_lat double precision;
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS dest_lon double precision;
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS src_lat  double precision;
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS src_lon  double precision;
