-- Read-only role for the API. The API only ever reads L1/L2 + freshness/run-log;
-- it must never see the raw_*/stg_* tables or be able to write. Run once as a
-- privileged user, then point the API's DATABASE_URL at wp_ro.
--   psql "$ADMIN_URL" -f db/grants.sql

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'wp_ro') THEN
    CREATE ROLE wp_ro LOGIN PASSWORD 'wp_ro';
  END IF;
END
$$;

GRANT USAGE ON SCHEMA public TO wp_ro;

-- exactly the tables the API reads
GRANT SELECT ON
  kpi_daily,
  kpi_heatmap_empty,
  kpi_baseline,
  kpi_target,
  data_freshness,
  etl_run_log
TO wp_ro;

-- defensively revoke anything broad and make sure no write/DDL is possible
REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON ALL TABLES IN SCHEMA public FROM wp_ro;
