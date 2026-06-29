-- Tier 1 of the terrain-aware OD cost: PURE empty-travel time keyed by the actual WORK-POINTS (fixed
-- yard blocks), not a 225m grid. Source = each cycle's empty-travel leg (origin = previous drop topos,
-- dest = this pickup topos) + the pure moving time (motion-segmented from truck_pos_hist) within that
-- leg's window. Filled by spawn_travel_aggregator. 30-day retention.
CREATE TABLE IF NOT EXISTS learn_travel_topos_sample (
  ytno        text        NOT NULL,
  leg_start   timestamptz NOT NULL,
  origin      text        NOT NULL,   -- work-point topos (e.g. 01PW-0809 block, or C54 crane)
  dest        text        NOT NULL,
  drive_s     int         NOT NULL,   -- pure moving time (s) for the leg
  captured_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (ytno, leg_start)
);
CREATE INDEX IF NOT EXISTS learn_travel_topos_sample_cap ON learn_travel_topos_sample (captured_at);
CREATE INDEX IF NOT EXISTS learn_travel_topos_sample_od  ON learn_travel_topos_sample (origin, dest);

-- Block↔block aggregation (dash heuristic: block codes carry a '-' like 01PW-0809; cranes like C54 do
-- not — and cranes MOVE so they must not be memorized by id). This is the tier-1 lookup the cost reads.
CREATE MATERIALIZED VIEW IF NOT EXISTS learn_travel_topos_pure AS
  SELECT origin, dest,
         percentile_cont(0.5) WITHIN GROUP (ORDER BY drive_s)::int AS p50_s,
         percentile_cont(0.9) WITHIN GROUP (ORDER BY drive_s)::int AS p90_s,
         count(*)::int AS n
    FROM learn_travel_topos_sample
   WHERE origin LIKE '%-%' AND dest LIKE '%-%'
   GROUP BY origin, dest;
CREATE UNIQUE INDEX IF NOT EXISTS learn_travel_topos_pure_od ON learn_travel_topos_pure (origin, dest);
