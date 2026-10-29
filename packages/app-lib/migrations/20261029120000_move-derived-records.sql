-- A downgrade removes this migration's record without restoring the tables,
-- so its replay must not fail on tables a previous run already dropped.
DROP TABLE IF EXISTS crash_analysis_mod_snapshots;
DROP TABLE IF EXISTS discovered_javas;
DROP TABLE IF EXISTS google_translate_ip_cache;
DROP TABLE IF EXISTS telemetry_outbox;
DROP TABLE IF EXISTS translation_cache;
