use alloy_primitives::U256;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use sqlx::{Row, SqlitePool};
use tempo_agentic_domain::ExecutionPlan;
use tempo_agentic_strategy::{
    DashboardData, DashboardStore, Level, LevelStore, Order, OrderState, OrderStore, Strategy,
    StrategyLevel, StrategyStore,
};

/// SQLite storage for strategy markets.
#[derive(Clone)]
pub struct SqliteStrategyStore {
    pool: SqlitePool,
}

impl SqliteStrategyStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

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

struct StrategyLevelRow {
    strategy_id: String,
    venue: String,
    chain: String,
    base_token: String,
    quote_token: String,
    level_id: String,
    side: String,
    trigger_price_usd: f64,
    amount: String,
    amount_decimals: i64,
    slippage_bps: i64,
}

impl StrategyLevelRow {
    fn from_row(row: sqlx::sqlite::SqliteRow) -> Result<Self> {
        Ok(Self {
            strategy_id: row.try_get("strategy_id")?,
            venue: row.try_get("venue")?,
            chain: row.try_get("chain")?,
            base_token: row.try_get("base_token")?,
            quote_token: row.try_get("quote_token")?,
            level_id: row.try_get("level_id")?,
            side: row.try_get("side")?,
            trigger_price_usd: row.try_get("trigger_price_usd")?,
            amount: row.try_get("amount")?,
            amount_decimals: row.try_get("amount_decimals")?,
            slippage_bps: row.try_get("slippage_bps")?,
        })
    }

    fn into_entry(self) -> Result<StrategyLevel> {
        let level_id = self.level_id;
        Ok(StrategyLevel {
            strategy: Strategy {
                id: self.strategy_id.clone(),
                venue: self.venue.parse().with_context(|| {
                    format!("strategy {} has an unusable venue", self.strategy_id)
                })?,
                chain: self.chain,
                base_token: self.base_token,
                quote_token: self.quote_token,
            },
            level: Level {
                strategy_id: self.strategy_id,
                side: self
                    .side
                    .parse()
                    .with_context(|| format!("level {level_id} has an unusable side"))?,
                trigger_price_usd: self.trigger_price_usd,
                amount: parse_u256(&self.amount, "amount", &level_id)?,
                amount_decimals: u8::try_from(self.amount_decimals).with_context(|| {
                    format!("level {level_id} has out-of-range amount_decimals")
                })?,
                slippage_bps: u16::try_from(self.slippage_bps)
                    .with_context(|| format!("level {level_id} has out-of-range slippage_bps"))?,
                id: level_id,
            },
        })
    }
}

fn strategy_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Strategy> {
    let id: String = row.try_get("id")?;
    Ok(Strategy {
        venue: row
            .try_get::<String, _>("venue")?
            .parse()
            .with_context(|| format!("strategy {id} has an unusable venue"))?,
        chain: row.try_get("chain")?,
        base_token: row.try_get("base_token")?,
        quote_token: row.try_get("quote_token")?,
        id,
    })
}

const STRATEGY_LEVEL_SELECT: &str = "SELECT s.id AS strategy_id, s.venue, s.chain, s.base_token, s.quote_token, \
            l.id AS level_id, l.side, l.trigger_price_usd, l.amount, \
            l.amount_decimals, l.slippage_bps \
     FROM levels l JOIN strategies s ON s.id = l.strategy_id";

struct OrderRow {
    id: String,
    level_id: String,
    venue: String,
    chain: String,
    token_in: String,
    token_out: String,
    reserved_amount: String,
    plan: String,
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
            plan: serde_json::from_str::<ExecutionPlan>(&self.plan)
                .with_context(|| format!("order {id} has an unusable plan"))?,
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

// Scalar U256 values are decimal; state JSON uses serde's hex encoding.
fn parse_u256(raw: &str, column: &str, row_id: &str) -> Result<U256> {
    raw.parse::<U256>()
        .with_context(|| format!("row {row_id} has a non-u256 {column} '{raw}'"))
}

#[async_trait]
impl StrategyStore for SqliteStrategyStore {
    async fn upsert_strategy(&self, strategy: &Strategy) -> Result<()> {
        let changed = sqlx::query(
            "INSERT INTO strategies( \
                 id, venue, chain, base_token, quote_token, created_at \
             ) VALUES (?, ?, ?, ?, ?, unixepoch()) \
             ON CONFLICT(id) DO UPDATE SET \
                 venue = excluded.venue, chain = excluded.chain, \
                 base_token = excluded.base_token, quote_token = excluded.quote_token \
             WHERE (strategies.venue = excluded.venue \
                    AND strategies.chain = excluded.chain \
                    AND strategies.base_token = excluded.base_token \
                    AND strategies.quote_token = excluded.quote_token) \
                OR NOT EXISTS (SELECT 1 FROM levels WHERE strategy_id = strategies.id)",
        )
        .bind(&strategy.id)
        .bind(strategy.venue.as_str())
        .bind(&strategy.chain)
        .bind(&strategy.base_token)
        .bind(&strategy.quote_token)
        .execute(&self.pool)
        .await
        .context("cannot upsert strategy")?
        .rows_affected();
        if changed != 1 {
            bail!("strategy {} cannot change while it has levels", strategy.id);
        }
        Ok(())
    }

    async fn get_strategy(&self, id: &str) -> Result<Option<Strategy>> {
        sqlx::query("SELECT id, venue, chain, base_token, quote_token FROM strategies WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("cannot read strategy")?
            .map(strategy_from_row)
            .transpose()
    }

    async fn list_strategies(&self) -> Result<Vec<Strategy>> {
        sqlx::query("SELECT id, venue, chain, base_token, quote_token FROM strategies ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .context("cannot list strategies")?
            .into_iter()
            .map(strategy_from_row)
            .collect()
    }
}

#[async_trait]
impl DashboardStore for SqliteStrategyStore {
    async fn dashboard_data(&self) -> Result<DashboardData> {
        let mut transaction = self.pool.begin().await?;
        let strategies = sqlx::query(
            "SELECT id, venue, chain, base_token, quote_token FROM strategies ORDER BY id",
        )
        .fetch_all(&mut *transaction)
        .await
        .context("cannot list dashboard strategies")?
        .into_iter()
        .map(strategy_from_row)
        .collect::<Result<Vec<_>>>()?;
        let levels = sqlx::query(&format!("{STRATEGY_LEVEL_SELECT} ORDER BY l.id"))
            .fetch_all(&mut *transaction)
            .await
            .context("cannot list dashboard levels")?
            .into_iter()
            .map(StrategyLevelRow::from_row)
            .map(|row| row.and_then(StrategyLevelRow::into_entry))
            .collect::<Result<Vec<_>>>()?;
        let orders = sqlx::query_as!(
            OrderRow,
            "SELECT id, level_id, venue, chain, token_in, token_out, reserved_amount, \
                    plan, state, swap_attempts, swap_retry_after_ts, created_at \
             FROM orders ORDER BY created_at, id"
        )
        .fetch_all(&mut *transaction)
        .await
        .context("cannot list dashboard orders")?
        .into_iter()
        .map(OrderRow::into_order)
        .collect::<Result<Vec<_>>>()?;
        transaction.commit().await?;

        Ok(DashboardData {
            strategies,
            levels,
            orders,
        })
    }
}

#[async_trait]
impl LevelStore for SqliteLevelStore {
    async fn upsert_level(&self, level: &Level, expected: &Strategy) -> Result<()> {
        if level.strategy_id != expected.id {
            bail!("level strategy_id does not match its expected strategy");
        }
        let changed = sqlx::query(
            "INSERT INTO levels( \
                 id, strategy_id, side, trigger_price_usd, amount, amount_decimals, \
                 slippage_bps, created_at \
             ) SELECT ?, id, ?, ?, ?, ?, ?, unixepoch() FROM strategies \
               WHERE id = ? AND venue = ? AND chain = ? \
                 AND base_token = ? AND quote_token = ? \
             ON CONFLICT(id) DO UPDATE SET \
                 strategy_id = excluded.strategy_id, side = excluded.side, \
                 trigger_price_usd = excluded.trigger_price_usd, amount = excluded.amount, \
                 amount_decimals = excluded.amount_decimals, \
                 slippage_bps = excluded.slippage_bps",
        )
        .bind(&level.id)
        .bind(level.side.as_str())
        .bind(level.trigger_price_usd)
        .bind(level.amount.to_string())
        .bind(i64::from(level.amount_decimals))
        .bind(i64::from(level.slippage_bps))
        .bind(&expected.id)
        .bind(expected.venue.as_str())
        .bind(&expected.chain)
        .bind(&expected.base_token)
        .bind(&expected.quote_token)
        .execute(&self.pool)
        .await
        .context("cannot upsert level")?
        .rows_affected();
        if changed != 1 {
            bail!("strategy {} changed while level was validated", expected.id);
        }
        Ok(())
    }

    async fn get_level(&self, id: &str) -> Result<Option<StrategyLevel>> {
        sqlx::query(&format!("{STRATEGY_LEVEL_SELECT} WHERE l.id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("cannot read level")?
            .map(StrategyLevelRow::from_row)
            .transpose()?
            .map(StrategyLevelRow::into_entry)
            .transpose()
    }

    async fn list_levels(&self) -> Result<Vec<StrategyLevel>> {
        let rows = sqlx::query(&format!("{STRATEGY_LEVEL_SELECT} ORDER BY l.id"))
            .fetch_all(&self.pool)
            .await
            .context("cannot list levels")?;
        rows.into_iter()
            .map(StrategyLevelRow::from_row)
            .map(|row| row.and_then(StrategyLevelRow::into_entry))
            .collect()
    }

    async fn delete_level(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM levels WHERE id = ?")
            .bind(id)
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
        // Denormalized columns keep filters out of state JSON.
        let status = order.status().as_str();
        let tx_hash = order.tx_hash();
        let plan = serde_json::to_string(&order.plan).context("cannot serialize order plan")?;
        let state = serde_json::to_string(&order.state).context("cannot serialize order state")?;
        let swap_attempts = i64::from(order.swap_attempts);
        sqlx::query!(
            "INSERT INTO orders( \
                 id, level_id, venue, chain, token_in, token_out, reserved_amount, \
                 status, tx_hash, plan, state, swap_attempts, swap_retry_after_ts, created_at \
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
                 level_id = excluded.level_id, \
                 venue = excluded.venue, \
                 chain = excluded.chain, \
                 token_in = excluded.token_in, \
                 token_out = excluded.token_out, \
                 reserved_amount = excluded.reserved_amount, \
                 status = excluded.status, \
                 tx_hash = excluded.tx_hash, \
                 plan = excluded.plan, \
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
            plan,
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
                    plan, state, swap_attempts, swap_retry_after_ts, created_at \
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
                    plan, state, swap_attempts, swap_retry_after_ts, created_at \
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
