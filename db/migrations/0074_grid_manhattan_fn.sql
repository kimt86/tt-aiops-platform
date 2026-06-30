-- Rotated-grid (29.8°) Manhattan distance in metres between two GPS points — the SQL twin of the
-- Rust quay_manhattan_m the dispatch cost uses. Trucks drive the yard grid, so this (not straight-line
-- haversine) is the realistic path length. Used by the Learning Center to show a meaningful effective
-- speed: the old "median speed" divided STRAIGHT-LINE distance by stop-inclusive time → ~6.9 km/h,
-- artificially slow (path is ~15% longer than straight-line). Grid distance ÷ realized time ≈ 8 km/h,
-- matching the cost model's MANHATTAN_SPEED (8.2 km/h). (Cruising/moving-only speed is ~24 km/h.)
CREATE OR REPLACE FUNCTION grid_manhattan_m(lat1 double precision, lon1 double precision,
                                            lat2 double precision, lon2 double precision)
RETURNS double precision LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
  SELECT abs( (lat2-lat1)*111320.0*0.86777 + (lon2-lon1)*111320.0*cos(radians((lat1+lat2)/2))*0.49697)
       + abs(-(lat2-lat1)*111320.0*0.49697 + (lon2-lon1)*111320.0*cos(radians((lat1+lat2)/2))*0.86777)
$$;
