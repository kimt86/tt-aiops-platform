-- db/seed/kpi_target.sql
-- Static KPI metadata. direction / tier / unit / display_name are known facts
-- (research definitions + mock tiers). target_value / excellent_value are LEFT NULL
-- on purpose: the mock's threshold numbers are in placeholder units that do NOT
-- match the adopted Phase-E research definitions (e.g. mock K_UTIL target 76% vs
-- research actuals ~97%; mock K_RTG_Q "s/HO" vs research YT_DIS->ACTV seconds).
-- Real thresholds require stakeholder sign-off against the KPI definition doc
-- (plan open decision #1). The mock values are kept in comments for reference.
--
-- Idempotent: safe to re-run.

INSERT INTO kpi_target (kpi_key, display_name, unit, direction, target_value, excellent_value, tier) VALUES
  -- mock placeholder: target 0.86 / excellent 0.77 km/Job
  ('K_EMPTY',   'Empty Travel Distance / Job', 'km/Job',  'LOWER_BETTER',  NULL, NULL, 'PRIMARY'),
  -- mock placeholder: target 40.3 / excellent 38.1 %
  ('K_EMPTY_R', 'Empty Travel Ratio',          '%',       'LOWER_BETTER',  NULL, NULL, 'PRIMARY_PARALLEL'),
  -- mock placeholder: target 16.5 / excellent 14.8 (s/HO); research unit = s (YT_DIS->ACTV)
  ('K_RTG_Q', 'QC Crane Wait Time',          's',       'LOWER_BETTER',  NULL, NULL, 'PRIMARY_KEY'),
  -- mock placeholder: target 31.3 / excellent 32.8 move/hr
  ('K_MPH',     'QC Moves Per Hour',           'move/hr', 'HIGHER_BETTER', NULL, NULL, 'REFERENCE'),
  -- mock placeholder: target 879 / excellent 786 s
  ('K_CYCLE',   'TT Cycle Time',               's',       'LOWER_BETTER',  NULL, NULL, 'TOP'),
  -- mock placeholder: target 76.0 / excellent 80.0 %
  ('K_UTIL',    'TT Utilization',              '%',       'HIGHER_BETTER', NULL, NULL, 'PHASE2')
ON CONFLICT (kpi_key) DO UPDATE SET
  display_name    = EXCLUDED.display_name,
  unit            = EXCLUDED.unit,
  direction       = EXCLUDED.direction,
  target_value    = EXCLUDED.target_value,
  excellent_value = EXCLUDED.excellent_value,
  tier            = EXCLUDED.tier;
