-- TOS-vs-ours dispatch comparison (shadow). When TOS assigns a truck to a work that we had also
-- recommended a truck for, record both choices and their arrival cost to the same work point, so we
-- can show — per divergence — WHY they differ and the resulting performance gap. Display/validation
-- only. our_* = our latest recommendation before TOS assigned; tos_* = what TOS actually did.
CREATE TABLE IF NOT EXISTS dispatch_compare_shadow (
  ts             timestamptz NOT NULL DEFAULT now(),
  qc             text NOT NULL,
  queuename      text NOT NULL,
  jobtype        text,
  tos_ytno       text NOT NULL,        -- truck TOS actually assigned
  tos_arrival_s  int,                  -- that truck's arrival cost to the work (live pos → work, OD)
  our_ytno       text,                 -- truck our matcher recommended
  our_arrival_s  int,                  -- our recommended truck's arrival cost
  agree          boolean,              -- same truck?
  reason         text,                 -- same | ours_closer | tos_closer
  delta_s        int,                  -- tos_arrival − our_arrival  (+ = we'd be faster)
  tos_upd        timestamptz NOT NULL, -- TOS assignment time (dedupe key)
  PRIMARY KEY (qc, queuename, tos_ytno, tos_upd)
);
CREATE INDEX IF NOT EXISTS dispatch_compare_ts ON dispatch_compare_shadow (ts);
