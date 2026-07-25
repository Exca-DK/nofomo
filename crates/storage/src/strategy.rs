use alloy_primitives::U256;
use anyhow::{Context, Result};
use async_trait::async_trait;
use sqlx::SqlitePool;
use tempo_agentic_strategy::{Level, LevelStore, Order, OrderState, OrderStore};

/// SQLite storage for the standing rules the daemon evaluates.
#[derive(Clone)]
pub struct SqliteLevelStore {
    pool: SqlitePool,
}

impl SqliteLevelStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// SQLite storage for execution attempts.
#[derive(Clone)]
pub struct SqliteOrderStore {
    pool: SqlitePool,
}

impl SqliteOrderStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

struct LevelRow {
    id: String,
    venue: String,
    chain: String,
    token_in: String,
    token_out: String,
    side: String,
    trigger_price_usd: f64,
    amount: String,
    amount_decimals: i64,
    slippage_bps: i64,
}

impl LevelRow {
    fn into_level(self) -> Result<Level> {
        let id = self.id;
        Ok(Level {
            venue: self
                .venue
                .parse()
                .with_context(|| format!("level {id} has an unusable venue"))?,
            chain: self.chain,
            token_in: self.token_in,
            token_out: self.token_out,
            side: self
                .side
                .parse()
                .with_context(|| format!("level {id} has an unusable side"))?,
            trigger_price_usd: self.trigger_price_usd,
            amount: parse_u256(&self.amount, "amount", &id)?,
            amount_decimals: u8::try_from(self.amount_decimals)
                .with_context(|| format!("level {id} has out-of-range amount_decimals"))?,
            slippage_bps: u16::try_from(self.slippage_bps)
                .with_context(|| format!("level {id} has out-of-range slippage_bps"))?,
            id,
        })
    }
}

struct OrderRow {
    id: String,
    level_id: String,
    venue: String,
    chain: String,
    token_in: String,
    token_out: String,
    reserved_amount: String,
    state: String,
    swap_attempts: i64,
    swap_retry_after_ts: Option<i64>,
    created_at: i64,
}

impl OrderRow {
    fn into_order(self) -> Result<Order> {
        let id = self.id;
        Ok(Order {
            level_id: self.level_id,
            venue: self
                .venue
                .parse()
                .with_context(|| format!("order {id} has an unusable venue"))?,
            chain: self.chain,
            token_in: self.token_in,
            token_out: self.token_out,
            reserved_amount: parse_u256(&self.reserved_amount, "reserved_amount", &id)?,
            state: serde_json::from_str::<OrderState>(&self.state)
                .with_context(|| format!("order {id} has an unusable state"))?,
            swap_attempts: u32::try_from(self.swap_attempts)
                .with_context(|| format!("order {id} has out-of-range swap_attempts"))?,
            swap_retry_after_ts: self.swap_retry_after_ts,
            created_at: self.created_at,
            id,
        })
    }
}

// Scalar U256 columns hold a decimal string. Amounts nested inside the `state`
// JSON are 0x-prefixed hex instead, because that is how serde encodes U256.
fn parse_u256(raw: &str, column: &str, row_id: &str) -> Result<U256> {
    raw.parse::<U256>()
        .with_context(|| format!("row {row_id} has a non-u256 {column} '{raw}'"))
}

#[async_trait]
impl LevelStore for SqliteLevelStore {
    async fn upsert_level(&self, level: &Level) -> Result<()> {
        let venue = level.venue.as_str();
        let side = level.side.as_str();
        let amount = level.amount.to_string();
        let amount_decimals = i64::from(level.amount_decimals);
        let slippage_bps = i64::from(level.slippage_bps);
        sqlx::query!(
            "INSERT INTO levels( \
                 id, venue, chain, token_in, token_out, side, trigger_price_usd, \
                 amount, amount_decimals, slippage_bps, created_at \
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, unixepoch()) \
             ON CONFLICT(id) DO UPDATE SET \
                 venue = excluded.venue, \
                 chain = excluded.chain, \
                 token_in = excluded.token_in, \
                 token_out = excluded.token_out, \
                 side = excluded.side, \
                 trigger_price_usd = excluded.trigger_price_usd, \
                 amount = excluded.amount, \
                 amount_decimals = excluded.amount_decimals, \
                 slippage_bps = excluded.slippage_bps",
            level.id,
            venue,
            level.chain,
            level.token_in,
            level.token_out,
            side,
            level.trigger_price_usd,
            amount,
            amount_decimals,
            slippage_bps,
        )
        .execute(&self.pool)
        .await
        .context("cannot upsert level")?;
        Ok(())
    }

    async fn get_level(&self, id: &str) -> Result<Option<Level>> {
        sqlx::query_as!(
            LevelRow,
            "SELECT id, venue, chain, token_in, token_out, side, trigger_price_usd, \
                    amount, amount_decimals, slippage_bps \
             FROM levels WHERE id = ?",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .context("cannot read level")?
        .map(LevelRow::into_level)
        .transpose()
    }

    async fn list_levels(&self) -> Result<Vec<Level>> {
        sqlx::query_as!(
            LevelRow,
            "SELECT id, venue, chain, token_in, token_out, side, trigger_price_usd, \
                    amount, amount_decimals, slippage_bps \
             FROM levels ORDER BY id"
        )
        .fetch_all(&self.pool)
        .await
        .context("cannot list levels")?
        .into_iter()
        .map(LevelRow::into_level)
        .collect()
    }

    async fn delete_level(&self, id: &str) -> Result<()> {
        sqlx::query!("DELETE FROM levels WHERE id = ?", id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("cannot delete level {id}; it may still have orders"))?;
        Ok(())
    }
}

#[async_trait]
impl OrderStore for SqliteOrderStore {
    async fn upsert_order(&self, order: &Order) -> Result<()> {
        let venue = order.venue.as_str();
        let reserved_amount = order.reserved_amount.to_string();
        // Denormalized query columns, derived so a reader never has to parse the
        // state JSON just to filter.
        let status = order.status().as_str();
        let tx_hash = order.tx_hash();
        let state = serde_json::to_string(&order.state).context("cannot serialize order state")?;
        let swap_attempts = i64::from(order.swap_attempts);
        sqlx::query!(
            "INSERT INTO orders( \
                 id, level_id, venue, chain, token_in, token_out, reserved_amount, \
                 status, tx_hash, state, swap_attempts, swap_retry_after_ts, created_at \
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
                 level_id = excluded.level_id, \
                 venue = excluded.venue, \
                 chain = excluded.chain, \
                 token_in = excluded.token_in, \
                 token_out = excluded.token_out, \
                 reserved_amount = excluded.reserved_amount, \
                 status = excluded.status, \
                 tx_hash = excluded.tx_hash, \
                 state = excluded.state, \
                 swap_attempts = excluded.swap_attempts, \
                 swap_retry_after_ts = excluded.swap_retry_after_ts",
            order.id,
            order.level_id,
            venue,
            order.chain,
            order.token_in,
            order.token_out,
            reserved_amount,
            status,
            tx_hash,
            state,
            swap_attempts,
            order.swap_retry_after_ts,
            order.created_at,
        )
        .execute(&self.pool)
        .await
        .context("cannot upsert order")?;
        Ok(())
    }

    async fn get_order(&self, id: &str) -> Result<Option<Order>> {
        sqlx::query_as!(
            OrderRow,
            "SELECT id, level_id, venue, chain, token_in, token_out, reserved_amount, \
                    state, swap_attempts, swap_retry_after_ts, created_at \
             FROM orders WHERE id = ?",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .context("cannot read order")?
        .map(OrderRow::into_order)
        .transpose()
    }

    async fn list_orders(&self) -> Result<Vec<Order>> {
        sqlx::query_as!(
            OrderRow,
            "SELECT id, level_id, venue, chain, token_in, token_out, reserved_amount, \
                    state, swap_attempts, swap_retry_after_ts, created_at \
             FROM orders ORDER BY created_at, id"
        )
        .fetch_all(&self.pool)
        .await
        .context("cannot list orders")?
        .into_iter()
        .map(OrderRow::into_order)
        .collect()
    }
}
