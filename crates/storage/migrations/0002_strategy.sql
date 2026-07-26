INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (2, unixepoch());

-- U256 token amounts use decimal TEXT because SQLite lacks 78-digit integers.
CREATE TABLE IF NOT EXISTS levels (
    id                TEXT    NOT NULL PRIMARY KEY,
    venue             TEXT    NOT NULL,
    chain             TEXT    NOT NULL,
    token_in          TEXT    NOT NULL,
    token_out         TEXT    NOT NULL,
    side              TEXT    NOT NULL CHECK (side IN ('buy', 'sell')),
    trigger_price_usd REAL    NOT NULL,
    amount            TEXT    NOT NULL,
    amount_decimals   INTEGER NOT NULL,
    slippage_bps      INTEGER NOT NULL,
    created_at        INTEGER NOT NULL
);

-- Each order snapshots its plan and market, while status and hash are query columns.
-- `reserved_amount` is decimal; U256 values inside state JSON use serde hex.
CREATE TABLE IF NOT EXISTS orders (
    id                  TEXT    NOT NULL PRIMARY KEY,
    level_id            TEXT    NOT NULL REFERENCES levels(id),
    venue               TEXT    NOT NULL,
    chain               TEXT    NOT NULL,
    token_in            TEXT    NOT NULL,
    token_out           TEXT    NOT NULL,
    reserved_amount     TEXT    NOT NULL,
    plan                TEXT    NOT NULL,
    status              TEXT    NOT NULL CHECK (status IN (
                            'pending', 'submitted', 'filled', 'failed', 'quarantined'
                        )),
    tx_hash             TEXT,
    state               TEXT    NOT NULL,
    swap_attempts       INTEGER NOT NULL DEFAULT 0,
    swap_retry_after_ts INTEGER,
    created_at          INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS orders_level_status ON orders(level_id, status);
CREATE INDEX IF NOT EXISTS orders_created_at ON orders(created_at);
