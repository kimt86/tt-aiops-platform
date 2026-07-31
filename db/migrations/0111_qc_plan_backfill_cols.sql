-- Columns the Oracle backfill can fill and the live path cannot, plus the provenance to tell the
-- two apart. public.live_workqueue carries only the nine plan fields (crates/extractor/sql/
-- workqueue.sql), so for rows captured live these stay NULL — that is the honest state, not a gap.
--
-- WHY EACH ONE:
--   act_dt      the per-bay ACTUAL work start. Measured 2026-07-30: min(ACT_DT) across a call lands
--               11-31 min after its berth time, and calls with a plan but no work yet have zero of
--               them. It is the authoritative anchor for lining a plan row up against the moves that
--               executed it — nothing else in our data says "this bay started here". We read it now
--               because it costs nothing extra on a row we are already fetching, and going back for
--               it later would mean a second pass over the same 174 calls.
--   cre_dt      NOT the plan issue time, despite the name. TOS lays down an empty bay skeleton per
--               call (CRE_USR_ID='CBAS_INTERFACE') two to three months before berthing, then fills
--               crane and quantity in by UPDATE — so issuing a plan creates no row and stamps no
--               time. Measured: CESA 011/2026 has all 64 rows at CRE_DT 05-22, berthing 07-30.
--               Stored for forensics, never as "when the plan appeared".
--   cre_usr     the one exception worth keeping: EDI-created rows ARE stamped at issue. WLHD 001/2026
--               had 80 rows created 07-30 09:49-12:03 with CRE_USR_ID='EDI', 25.3h before berthing,
--               totals matching its declared counts exactly. So cre_dt means "issue time" only where
--               this column says EDI, and this column is how a reader knows which case they hold.
--   upd_dt      last touch. Also the column the live 90s query filters on — and there is no index on
--               it, which is why that query scans the table while a (vessel, voyage) lookup seeks.
ALTER TABLE scenario.qc_plan ADD COLUMN IF NOT EXISTS act_dt  timestamptz; -- bay work actually began
ALTER TABLE scenario.qc_plan ADD COLUMN IF NOT EXISTS cre_dt  timestamptz; -- skeleton created (see above)
ALTER TABLE scenario.qc_plan ADD COLUMN IF NOT EXISTS upd_dt  timestamptz; -- last touched
ALTER TABLE scenario.qc_plan ADD COLUMN IF NOT EXISTS cre_usr text;        -- 'EDI' => cre_dt IS issue time

-- Backfilled rows are the plan's FINAL edited state, not the state at any past instant. A live rev
-- chain shows a call being revised (measured: 682 of 1,657 queues revised, up to 13 times, 118 of
-- them changing quantity); a backfilled row collapses all of that into one. Consumers comparing
-- "what was planned" across calls must know which kind they are holding, so qc_plan_call.source
-- carries it and this comment records why it matters.
COMMENT ON COLUMN scenario.qc_plan.act_dt IS
  'Per-bay actual work start (TOS ACT_DT). Backfill only — live_workqueue does not carry it.';
