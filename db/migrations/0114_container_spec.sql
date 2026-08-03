-- Container size, so landside volume can be stated in TEU instead of only in box counts.
--
-- THE GAP. The gate stream says how many boxes crossed, never how big they were. Asked for TEU we
-- could only answer in moves. scenario.container carries iso/size but it is a VESSEL manifest keyed
-- by (vessel, voyage) — join gate containers to it and only 47.9% match even ignoring the voyage,
-- because export and empty boxes arriving by road were never on a discharge manifest we hold.
--
-- WHY A DICTIONARY AND NOT A COLUMN. A box's ISO type is a permanent physical property: once known,
-- known forever, and the same box comes back. So this is keyed by container number alone and
-- accumulates. That is also why it has NO retention — dropping a row means paying Oracle again for
-- something that cannot have changed. Growth is bounded in practice by how many distinct boxes this
-- terminal ever sees: ~4,000 new unknowns a day today, falling as repeats accumulate, so on the
-- order of 30MB a year. Left to grow on purpose.
--
-- WHERE THE MISSING HALF COMES FROM. TOSADM.CYY_CONTAINER.CYY_CONT_ISO — verified 2026-08-03 by
-- dictionary probe that CYC_HISTORY (what the gate collector already reads) and JOB_ORDER_LIST carry
-- no size column at all, so there is no free ride on an existing query. Lookups go by container
-- number, which is an index seek: 200 numbers in 2.08s including the ~0.9s SSH round trip.
--
-- ★TIMING IS THE WHOLE DESIGN. CYY_CONTAINER is CURRENT yard inventory, so a box that has left is
-- simply absent. Measured hit rate against it: 91.4% for containers seen in the last 3 hours, 77%
-- for the last 3 days. Look up promptly and the answer is there; let it age and it is gone. This is
-- why nothing here backfills — chasing old unknowns would spend Oracle on exactly the population
-- least likely to answer.
--
-- Expected coverage from the day it starts: ~48% locally (seed below) plus ~91% of the rest = ~95%.
-- Windows BEFORE it starts keep the 48%, which is why the output publishes a coverage percentage
-- rather than a bare TEU total that would silently read as the truth.

CREATE TABLE IF NOT EXISTS scenario.container_spec (
    contno     text PRIMARY KEY,
    iso        text,        -- ISO 6346 size/type code as TOS holds it: '22G1', '45G1', 'L0T4'
    size       text,        -- 'twenty' | 'forty' | 'forty_five'
    conttype   text,        -- TOS category: DV dry / RE reefer / OT open-top / CT tank
    source     text NOT NULL,  -- 'manifest' (local, authoritative) | 'yard' (CYY_CONTAINER lookup)
    first_seen timestamptz NOT NULL DEFAULT now()
);

-- Size from the ISO code's first character. Measured over the 382,297 manifest rows we already hold,
-- where TOS's own size label is available to check against:
--   4 -> forty 250,335 · 2 -> twenty 129,519 · L -> forty_five 1,893 · 9/5/1/P -> forty 520 · M -> forty_five 30
-- The 9/5/1/P group is TOS's own classification rather than the ISO length code, which is why this
-- follows the observed mapping and not the standard alone. NULL for anything unrecognised: a wrong
-- size silently doubles or halves a TEU total, and no answer is better than a confident wrong one.
CREATE OR REPLACE FUNCTION scenario.iso_size(iso text) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE substr($1, 1, 1)
           WHEN '2' THEN 'twenty'
           WHEN '4' THEN 'forty'
           WHEN '9' THEN 'forty'
           WHEN '5' THEN 'forty'
           WHEN '1' THEN 'forty'
           WHEN 'P' THEN 'forty'
           WHEN 'L' THEN 'forty_five'
           WHEN 'M' THEN 'forty_five'
         END
$$;

-- TEU from size. 45ft counts as 2, the usual terminal-throughput convention — it is a CHOICE, which
-- is why it lives in one named place instead of being spread through the output queries.
CREATE OR REPLACE FUNCTION scenario.size_teu(size text) RETURNS numeric
LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE $1 WHEN 'twenty' THEN 1.0 WHEN 'forty' THEN 2.0 WHEN 'forty_five' THEN 2.0 END::numeric
$$;

-- Seed from what we already hold. This is NOT a backfill — no Oracle, no historical chase, just the
-- manifest rows already sitting in this database. It covers about half of gate containers on day
-- one, and the collector only ever has to ask about the rest.
INSERT INTO scenario.container_spec (contno, iso, size, source, first_seen)
SELECT DISTINCT ON (contno) contno, iso, size, 'manifest', now()
  FROM scenario.container
 WHERE contno IS NOT NULL AND size IS NOT NULL
 ORDER BY contno, iso NULLS LAST
ON CONFLICT (contno) DO NOTHING;
