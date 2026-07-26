CREATE TABLE process_sessions (
    id TEXT PRIMARY KEY,
    version TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER
);

CREATE TABLE research_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES process_sessions(id),
    request_json TEXT NOT NULL,
    result_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE quotes (
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

CREATE TABLE execution_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    quote_id TEXT NOT NULL REFERENCES quotes(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN (
        'claimed', 'expired', 'rejected_unconfirmed', 'confirmed', 'failed'
    )),
    error_class TEXT,
    created_at INTEGER NOT NULL,
    finished_at INTEGER
);

CREATE TABLE transaction_refs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id INTEGER NOT NULL REFERENCES execution_attempts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    chain TEXT NOT NULL,
    tx_id TEXT NOT NULL,
    status TEXT NOT NULL,
    UNIQUE(attempt_id, kind, tx_id)
);

-- SQLite only implies NOT NULL for INTEGER primary keys, so text ones say it.
CREATE TABLE strategies (
    id TEXT NOT NULL PRIMARY KEY,
    venue TEXT NOT NULL,
    chain TEXT NOT NULL,
    base_token TEXT NOT NULL,
    quote_token TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    CHECK (lower(base_token) <> lower(quote_token))
);

-- U256 token amounts use decimal TEXT because SQLite lacks 78-digit integers.
CREATE TABLE levels (
    id TEXT NOT NULL PRIMARY KEY,
    strategy_id TEXT NOT NULL REFERENCES strategies(id),
    side TEXT NOT NULL CHECK (side IN ('buy', 'sell')),
    trigger_price_usd REAL NOT NULL,
    amount TEXT NOT NULL,
    amount_decimals INTEGER NOT NULL,
    slippage_bps INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

-- Orders retain the complete execution market even if authoring data later changes.
CREATE TABLE orders (
    id TEXT NOT NULL PRIMARY KEY,
    level_id TEXT NOT NULL REFERENCES levels(id),
    venue TEXT NOT NULL,
    chain TEXT NOT NULL,
    token_in TEXT NOT NULL,
    token_out TEXT NOT NULL,
    reserved_amount TEXT NOT NULL,
    plan TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'pending', 'submitted', 'filled', 'failed', 'quarantined'
    )),
    tx_hash TEXT,
    state TEXT NOT NULL,
    swap_attempts INTEGER NOT NULL DEFAULT 0,
    swap_retry_after_ts INTEGER,
    created_at INTEGER NOT NULL
);

CREATE INDEX quotes_session_status ON quotes(session_id, status);
CREATE INDEX quotes_created_at ON quotes(created_at);
CREATE INDEX research_created_at ON research_runs(created_at);
CREATE INDEX levels_strategy ON levels(strategy_id);
CREATE INDEX orders_level_status ON orders(level_id, status);
CREATE INDEX orders_created_at ON orders(created_at);

-- The version marker must be the final statement in the initialization transaction.
PRAGMA user_version = 3;
