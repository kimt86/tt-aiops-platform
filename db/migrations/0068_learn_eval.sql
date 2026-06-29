-- Periodic learning evaluation: tracks travel-time prediction ACCURACY over time (is the model
-- getting better as data accumulates?) + data-accumulation volumes. Powers the Learning Center's
-- "learning status" view. Logged hourly by spawn_learn_eval (livemap.rs). 90-day retention.
CREATE TABLE IF NOT EXISTS learn_eval (
  ts            timestamptz NOT NULL DEFAULT now(),
  n_legs        int,     -- holdout legs evaluated (recent empty-travel, pure drive time)
  od_mae_s      int,     -- OD-pure model MAE (s)   (zone225_pure p50, else Manhattan/6.61)
  od_mape       real,    -- OD-pure model MAPE (%)
  manh_mae_s    int,     -- Manhattan baseline MAE (s)
  manh_mape     real,    -- Manhattan baseline MAPE (%)
  hifreq_pts    bigint,  -- 3s GPS accumulation (truck_pos_hifreq)
  drive_samples bigint,  -- pure-driving leg samples (learn_travel_drive_sample)
  pure_pairs    int,     -- pure OD coverage (zone225_pure n>=10)
  PRIMARY KEY (ts)
);
