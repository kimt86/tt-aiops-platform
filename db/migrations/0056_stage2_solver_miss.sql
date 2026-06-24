-- Phase-2 adoption: the recommendation is now the deadline-aware min-cost optimum (not greedy). Track
-- deadline misses for BOTH so we can confirm the optimal doesn't trade away deadline protection for
-- efficiency. A "miss" = the assigned truck's conservative (p90) arrival is after the work-ETA.
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS greedy_miss  int;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS optimal_miss int;
