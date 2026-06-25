-- Truck position + dispatch-state history (every ~30s, from the live websocket GPS — no TOS load).
-- Lets the TOS-vs-ours comparison reconstruct the truck pool AT the dispatch moment (T1=upd_ts),
-- eliminating the timing skew (our 60s recommendation tick ≠ TOS's assignment instant).
CREATE TABLE IF NOT EXISTS truck_pos_hist (
  ts    timestamptz NOT NULL,
  ytno  text NOT NULL,
  lat   double precision,
  lon   double precision,
  state text,            -- idle | soon_idle | approaching | wait_rtg | delivering | empty_travel | staging
  PRIMARY KEY (ytno, ts)
);
CREATE INDEX IF NOT EXISTS truck_pos_hist_ts ON truck_pos_hist (ts);
