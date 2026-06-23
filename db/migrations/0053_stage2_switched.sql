-- Stage-2 stability: flag whether each recommendation switched the vehicle to a DIFFERENT work
-- bucket vs the previous tick. The matcher applies a switch penalty (a vehicle keeps its prior
-- bucket unless another is meaningfully cheaper), and logs `switched` so thrash rate is measurable.
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS switched boolean;
