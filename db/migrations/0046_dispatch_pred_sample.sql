-- Dispatch Stage-1 prediction shadow log: each row records, for a container near the front of a
-- QC's work, what we PREDICTED (when the QC would work it = pred_work_eta_ts; the dispatch deadline
-- = work-ETA − lead; whether it had a truck; the QC's slack) at logged_at. resolved_at is filled in
-- when the container leaves the live work pool (≈ actually worked) = the ground truth. Evaluation:
--   (A) accuracy = resolved_at − pred_work_eta_ts ;  (B) effect: late+unassigned rows vs real QC
--   starvation (qc_wait_sample). Each container logged once while open (deduped on unresolved rows).
CREATE TABLE IF NOT EXISTS dispatch_pred_sample (
  id                   bigserial PRIMARY KEY,
  logged_at            timestamptz NOT NULL DEFAULT now(),
  qc                   text NOT NULL,
  vessel               text,
  contno               text NOT NULL,
  queuename            text,
  jobtype              text,                 -- 'DS' | 'LD'
  pred_work_eta_ts     timestamptz,          -- predicted: when the QC works this container
  dispatch_deadline_ts timestamptz,          -- pred_work_eta − lead (latest sensible dispatch)
  assigned             boolean NOT NULL,     -- had a truck (ytno) at log time
  slack_s              int,                  -- QC slack (s) at log time
  lead_s               int,                  -- dispatch lead used (DS 300 / LD 1200)
  resolved_at          timestamptz           -- left the pool ≈ actually worked (NULL until then)
);
CREATE INDEX IF NOT EXISTS dispatch_pred_open ON dispatch_pred_sample (contno) WHERE resolved_at IS NULL;
CREATE INDEX IF NOT EXISTS dispatch_pred_logged ON dispatch_pred_sample (logged_at);
