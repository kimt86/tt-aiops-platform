-- PHASE-2 SHADOW: per-tick comparison of the greedy solver vs the true min-cost-flow optimum on the
-- SAME cost matrix. Measures how much total arrival-time the greedy assignment leaves on the table
-- (gap_pct) before we decide whether the optimal solver is worth adopting. Display/validation only.
CREATE TABLE IF NOT EXISTS stage2_solver_shadow (
  ts             timestamptz NOT NULL DEFAULT now(),
  tick           bigint,
  n_trucks       int,       -- candidate vehicles this tick
  n_works        int,       -- candidate work buckets this tick
  greedy_n       int,       -- assignments the greedy solver made
  greedy_cost_s  bigint,    -- Σ arrival_s over the greedy assignment
  optimal_n      int,       -- assignments the min-cost-flow optimum made (max matching)
  optimal_cost_s bigint,    -- Σ arrival_s over the optimal assignment
  gap_pct        real,      -- (greedy_cost − optimal_cost) / optimal_cost × 100  (≥0; 0 = greedy optimal)
  PRIMARY KEY (ts)
);
CREATE INDEX IF NOT EXISTS stage2_solver_ts ON stage2_solver_shadow (ts);
