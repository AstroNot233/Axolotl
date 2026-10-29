CREATE TABLE crash_analysis_mod_snapshots (
	instance_id TEXT NOT NULL,
	filename TEXT NOT NULL,
	size INTEGER NOT NULL,
	sha256 TEXT NOT NULL,
	project_id TEXT,
	project_title TEXT,
	icon_url TEXT,
	version_id TEXT,
	version_number TEXT,
	PRIMARY KEY (instance_id, filename)
);

CREATE TABLE discovered_javas (
	path TEXT NOT NULL PRIMARY KEY,
	major_version INTEGER NOT NULL,
	full_version TEXT NOT NULL,
	architecture TEXT NOT NULL,
	file_size INTEGER NOT NULL,
	file_mtime_ms INTEGER NOT NULL,
	distribution TEXT
);

CREATE INDEX discovered_javas_major_version
	ON discovered_javas (major_version);

CREATE TABLE google_translate_ip_cache (
	ip TEXT NOT NULL PRIMARY KEY,
	latency_ms INTEGER NOT NULL,
	created_at INTEGER NOT NULL
);

CREATE TABLE telemetry_outbox (
	event_id TEXT NOT NULL,
	event_type TEXT NOT NULL CHECK (event_type IN ('heartbeat', 'error')),
	payload JSONB NOT NULL,
	created_at INTEGER NOT NULL,
	next_attempt_at INTEGER NOT NULL,
	attempts INTEGER NOT NULL DEFAULT 0,
	size_bytes INTEGER NOT NULL,
	dedupe_key TEXT NOT NULL,
	occurrence_count INTEGER NOT NULL DEFAULT 1,
	PRIMARY KEY (event_id),
	UNIQUE (dedupe_key)
);

CREATE INDEX telemetry_outbox_retry
	ON telemetry_outbox (next_attempt_at, created_at);

CREATE TABLE translation_cache (
	key TEXT NOT NULL PRIMARY KEY,
	translation TEXT NOT NULL,
	created_at INTEGER NOT NULL
);

CREATE INDEX translation_cache_created_at
	ON translation_cache (created_at);
