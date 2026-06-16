-- Hourly terminal weather (Port Klang / Westports ≈ 2.9252,101.2927) from Open-Meteo
-- (free, no API key). A travel-time feature: rain / high wind / low visibility slow yard
-- driving. Ingested hourly by the `weather` extractor subcommand (wp-weather.timer); upserts
-- past_days=1 + forecast each run so brief gaps self-heal. ts = hour bucket in UTC, joined to
-- travel samples by date_trunc('hour', dropped_at). No Oracle. See research/travel-time.
CREATE TABLE IF NOT EXISTS weather_hourly (
  ts            TIMESTAMPTZ PRIMARY KEY,   -- hour (UTC)
  precip_mm     DOUBLE PRECISION,          -- precipitation in that hour (mm)
  wind_kmh      DOUBLE PRECISION,          -- 10m wind speed (km/h)
  visibility_m  DOUBLE PRECISION,          -- visibility (m)
  weather_code  INT,                       -- WMO weather code
  captured_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
