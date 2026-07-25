CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (1, unixepoch());

CREATE TABLE IF NOT EXISTS process_sessions (
    id TEXT PRIMARY KEY,
    version TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER
);

CREATE TABLE IF NOT EXISTS research_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES process_sessions(id),
    request_json TEXT NOT NULL,
    result_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS quotes (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES process_sessions(id),
    venue TEXT NOT NULL,
    chain TEXT NOT NULL,
    request_json TEXT NOT NULL,
    view_json TEXT NOT NULL,
    plan_digest TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'quoted', 'claimed', 'expired', 'rejected_unconfirmed',
        'invalidated_restart', 'confirmed', 'failed'
    )),
    created_at INTEGER NOT NULL,
    consumed_at INTEGER
);

CREATE TABLE IF NOT EXISTS execution_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    quote_id TEXT NOT NULL REFERENCES quotes(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN (
        'claimed', 'expired', 'rejected_unconfirmed', 'confirmed', 'failed'
    )),
    error_class TEXT,
    created_at INTEGER NOT NULL,
    finished_at INTEGER
);

CREATE TABLE IF NOT EXISTS transaction_refs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id INTEGER NOT NULL REFERENCES execution_attempts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    chain TEXT NOT NULL,
    tx_id TEXT NOT NULL,
    status TEXT NOT NULL,
    UNIQUE(attempt_id, kind, tx_id)
);

CREATE INDEX IF NOT EXISTS quotes_session_status ON quotes(session_id, status);
CREATE INDEX IF NOT EXISTS quotes_created_at ON quotes(created_at);
CREATE INDEX IF NOT EXISTS research_created_at ON research_runs(created_at);
