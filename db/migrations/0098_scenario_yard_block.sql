-- block_id -> human block name map (e.g. 202 -> '02T'), for labelling reconstructed yard cells in
-- scenario output. Populated from CYY_CONTAINER (DISTINCT CRNT_PSN_IDX_NO1 <-> block-from-CLOCATION)
-- during the yard snapshot step (reuses that CYY scan — no extra Oracle load).
CREATE TABLE IF NOT EXISTS scenario.yard_block (
  block_id   INTEGER PRIMARY KEY,
  block      TEXT NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
