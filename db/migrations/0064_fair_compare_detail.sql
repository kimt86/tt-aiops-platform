-- Per-pair detail behind the fair-comparison headline, so the ~25% empty-travel saving can be broken
-- down (by jobtype DS/LD, crane, hour, distance) and bias-checked (distribution, % where WE are worse).
-- One row per TOS truck→work assignment in each 5-min run: tos_s = TOS's actual empty travel for that
-- pair; our_s = the travel that truck got under our optimal re-matching of the same instant's pool.
-- Saving for the pair = tos_s − our_s (negative = our matching made that truck worse). 7-day retention.
CREATE TABLE IF NOT EXISTS fair_compare_detail (
  ts      timestamptz NOT NULL DEFAULT now(),
  jobtype text        NOT NULL,   -- 'DS' | 'LD'
  qc      text        NOT NULL,   -- crane of the TOS-assigned work
  tos_s   int         NOT NULL,   -- TOS empty-travel seconds (diagonal)
  our_s   int         NOT NULL    -- our re-matched empty-travel seconds
);
CREATE INDEX IF NOT EXISTS fair_compare_detail_ts ON fair_compare_detail (ts);
