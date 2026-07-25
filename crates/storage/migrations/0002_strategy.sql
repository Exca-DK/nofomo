INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (2, unixepoch());

-- Standing rules the daemon evaluates against a price feed. Amounts are U256
-- base units of token_in; SQLite has no NUMERIC(78,0), so they are stored as
-- decimal TEXT and parsed back into U256 in Rust.
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

-- One execution attempt per fired level. `state` holds the JSON-encoded
-- OrderState; `status` and `tx_hash` are denormalized query columns derived
-- from it at write time and never read back. Venue, chain, and the token pair
-- are snapshotted so editing the level cannot change what the order committed.
-- Note the two U256 encodings: `reserved_amount` is decimal, but amounts nested
-- inside the `state` JSON are 0x-prefixed hex (how serde serializes U256).
CREATE TABLE IF NOT EXISTS orders (
    id                  TEXT    NOT NULL PRIMARY KEY,
    level_id            TEXT    NOT NULL REFERENCES levels(id),
    venue               TEXT    NOT NULL,
    chain               TEXT    NOT NULL,
    token_in            TEXT    NOT NULL,
    token_out           TEXT    NOT NULL,
    reserved_amount     TEXT    NOT NULL,
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
