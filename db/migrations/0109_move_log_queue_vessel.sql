-- Carry the stowage-plan label and the vessel identity on the crane handover logs themselves.
--
-- WHY. Both move logs come from TOSADM.MCH_OPERATION, which already holds MCH_OPER_QUEUENAME
-- ("18H-L" = 40ft bay 18 / Hold / Load), MCH_OPER_VESSEL and MCH_OPER_VOYAGE — we simply were not
-- selecting them. Everything downstream therefore reconstructs those facts from elsewhere, and both
-- detours are load-bearing on something that expires:
--   * the bay label survives ONLY in 21-day sliding shadow logs (dispatch_pred_sample and three
--     siblings, pruned in crates/api/src/workpool.rs / livemap.rs). Every one of them starts exactly
--     21 days ago. No permanent record anywhere ties a COMPLETED move to a ship bay.
--   * vessel/voyage come only from scenario.move_hist, which starts 2026-07-21 and holds DS/LD only,
--     so 85% of qc_move_log and 91% of rtg_move_log rows currently have no vessel at all.
-- Reading these three from the row we are already fetching costs no extra Oracle work: the columns
-- live in the same row piece, the WHERE clause and PK-seek access path do not change, and the plan
-- cannot shift (the INDEX hint pins it). Measured 2026-07-30: 14 columns x 16,732 rows in 1.373s,
-- against live ticks of 36-68 rows at ~1.05s — the cost is the SSH round trip, not the payload.
--
-- NULL IS A NORMAL VALUE HERE, not a gap to chase:
--   * queuename is absent BY DESIGN for AH / GI / GO moves (re-stow and gate work belongs to no
--     ship bay). Measured over a 6h window: RTG 80.3% populated, ES 90.9%, QC 100.0%, and every
--     missing row is exactly one of those three job types.
--   * yard-crane rows carry two different shapes: LD/DS/LC use the ship-bay form ("26D-L") — a yard
--     crane labels itself with the bay it is feeding — while RH/MI/MO carry a yard-internal id
--     ("YY260729233340"). Consumers must not assume one grammar.
--   * existing rows stay NULL. The live insert path is ON CONFLICT DO NOTHING, so re-reading old
--     keys does NOT fill them; backfill needs its own pass and is deliberately not part of this.
--
-- vessel/voyage ARE ALWAYS PRESENT, AND SOMETIMES A SENTINEL. Verified 2026-07-30 against
-- scenario.vessel_call and live_vessel_schedule: RHXX (on RH/MI/MO) and ATGO/ATLD/ATMO/ATRH (on AH,
-- where the suffix encodes the job the re-stow serves) exist in neither table, while every code
-- appearing on DS/LD/GI/GO exists in the live schedule. So "vessel IS NOT NULL" is NOT the same as
-- "this move belongs to a ship" — grouping without excluding those five codes invents phantom
-- vessels. Worth noting the upside: gate moves (GI/GO) carry real vessel identity, which nothing
-- downstream has today.
--
-- NO CHECK CONSTRAINTS, ON PURPOSE. A move-log poll is one transaction for the whole batch and
-- propagates insert errors with `?`, so a single violating row would roll the batch back and leave
-- the watermark unadvanced — the row is then re-read every tick and the stream stalls for good, with
-- no self-healing. scenario.yard_move can afford the constraints that 0106 added because scengen is
-- isolated and forces Ok(()); these two tables are the critical path. Validate in Rust, keep the row.
--
-- Widening only, nullable, no default -> metadata-only on PG 11+ (server is 17.10), so this is an
-- instant catalog change on rtg_move_log (509MB / 1.9M rows) and qc_move_log (125MB / 436k rows).
-- Nothing depends on these tables' shape: no views, matviews, rules, triggers, FKs or functions
-- reference them (checked via pg_depend/pg_rewrite/pg_trigger/pg_constraint/pg_proc), and no
-- consumer does SELECT * against the base tables. Same shape of change as 0089 (status column).
--
-- Ordering matters: apply this BEFORE deploying the extractor that writes these columns. The old
-- binary ignores the new columns (its INSERT names its columns explicitly), but a new binary against
-- the old schema fails every tick on undefined_column, freezing BOTH move streams — and nothing
-- alerts on that today, so it would be silent until a downstream 90-minute window starts losing
-- data permanently.
ALTER TABLE qc_move_log  ADD COLUMN IF NOT EXISTS queuename TEXT; -- "18H-L" = bay 18 / Hold / Load
ALTER TABLE qc_move_log  ADD COLUMN IF NOT EXISTS vessel    TEXT;
ALTER TABLE qc_move_log  ADD COLUMN IF NOT EXISTS voyage    TEXT;
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS queuename TEXT; -- ship-bay form OR yard id "YY…"
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS vessel    TEXT;
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS voyage    TEXT;
