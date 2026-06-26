-- Fair head-to-head: over a recent window of TOS dispatch DECISIONS (same trucks, same works, same
-- positions), compare TOS's actual matching to OUR solver's optimal 1:1 matching (truck=work,
-- reservation-respected). Replaces the optimistic per-work "closest available truck" metric, which
-- double-books the globally-nearest truck across many works and so overstates our advantage.
CREATE TABLE IF NOT EXISTS fair_compare_shadow (
  ts          timestamptz PRIMARY KEY DEFAULT now(),
  window_min  int    NOT NULL,        -- the window of TOS assignments matched
  n           int    NOT NULL,        -- batch size (truck↔work pairs)
  tos_total_s bigint NOT NULL,        -- TOS matching: total empty-travel (sum over its actual assignments)
  our_total_s bigint NOT NULL,        -- our optimal matching: total empty-travel on the SAME trucks+works
  savings_pct double precision NOT NULL,  -- (tos_total - our_total) / tos_total
  same_n      int    NOT NULL         -- pairs where our optimal picks the SAME truck TOS did
);
