-- Live 1-minute terminal weather from Tomorrow.io (no credit card; free 500/day·25/hr). One
-- POST to /v4/timelines (timestep 1m, a short past window) returns a 1-minute series, so polling
-- every ~3 min (≈480/day, 20/hr — both under the free caps) gives full 1-minute coverage AND a
-- fresh value for the live map. Targets intermittent squalls that hourly/15-min averages miss.
-- Source is a model nowcast (not a gauge) — keep alongside radar/observed later if needed.
CREATE TABLE IF NOT EXISTS weather_1min (
  ts            TIMESTAMPTZ PRIMARY KEY,   -- minute (UTC)
  precip_mm_hr  DOUBLE PRECISION,          -- precipitationIntensity (mm/hr)
  visibility_km DOUBLE PRECISION,          -- visibility (km) — squalls crash this
  wind_ms       DOUBLE PRECISION,          -- windSpeed (m/s)
  weather_code  INT,                       -- Tomorrow.io weather code
  captured_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS weather_1min_ts_idx ON weather_1min (ts DESC);
