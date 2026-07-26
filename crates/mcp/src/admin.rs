use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ErrorData as McpError, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{Json, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tempo_agentic_price::{PricePair, PriceSource};
use tempo_agentic_strategy::{
    DashboardStore, LevelStore, Order, OrderStore, Strategy, StrategyLevel, StrategyStore,
    trade_direction,
};
use tempo_agentic_trigger::{
    FeedSnapshot, LevelDraft, RuntimeLevelState, RuntimeStatus, StrategyDraft, TokenResolver,
    validate_level, validate_strategy,
};

/// Orders returned when the caller does not say how many it wants.
const DEFAULT_ORDER_LIMIT: usize = 50;

/// MCP handler for the running daemon's live database.
#[derive(Clone)]
pub struct AdminHandler {
    strategies: Arc<dyn StrategyStore>,
    levels: Arc<dyn LevelStore>,
    orders: Arc<dyn OrderStore>,
    dashboard: DashboardDeps,
    tokens: Arc<TokenResolver>,
    max_slippage_bps: u16,
    /// Checks price support before storing rules.
    prices: Arc<dyn PriceSource>,
    tool_router: ToolRouter<Self>,
}

#[derive(Clone)]
pub struct DashboardDeps {
    pub store: Arc<dyn DashboardStore>,
    pub runtime: Arc<RuntimeStatus>,
}

impl AdminHandler {
    pub fn new(
        strategies: Arc<dyn StrategyStore>,
        levels: Arc<dyn LevelStore>,
        orders: Arc<dyn OrderStore>,
        dashboard: DashboardDeps,
        tokens: Arc<TokenResolver>,
        max_slippage_bps: u16,
        prices: Arc<dyn PriceSource>,
    ) -> Self {
        Self {
            strategies,
            levels,
            orders,
            dashboard,
            tokens,
            max_slippage_bps,
            prices,
            tool_router: Self::tool_router(),
        }
    }

    /// Builds the observational JSON view after durable data has left its read transaction.
    pub async fn dashboard_snapshot(&self) -> anyhow::Result<DashboardSnapshot> {
        let durable = self.dashboard.store.dashboard_data().await?;
        // Eventual consistency is intentional: runtime is copied only after the DB transaction.
        let runtime = self.dashboard.runtime.snapshot();
        let resolver = self.tokens.as_ref();
        let levels = durable
            .levels
            .iter()
            .map(|entry| {
                dashboard_level_view(
                    entry,
                    runtime.level_state(&entry.level.id, &durable.orders),
                    resolver,
                )
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let orders = durable
            .orders
            .iter()
            .rev()
            .take(DEFAULT_ORDER_LIMIT)
            .map(order_view)
            .collect();

        Ok(DashboardSnapshot {
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: runtime.started_at,
            generated_at: runtime.generated_at,
            allow_broadcast: runtime.allow_broadcast,
            strategies: durable.strategies.iter().map(strategy_view).collect(),
            levels,
            orders,
            feeds: runtime.feeds,
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DaemonStatus {
    pub version: String,
    /// Whether the daemon may broadcast signed transactions.
    pub allow_broadcast: bool,
    pub levels: usize,
    /// Order counts by status: pending, submitted, filled, failed, quarantined.
    pub orders: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LevelView {
    pub id: String,
    pub strategy_id: String,
    pub venue: String,
    pub chain: String,
    pub side: String,
    pub token_in: String,
    pub token_out: String,
    pub trigger_price_usd: f64,
    /// Base units of `token_in`, alongside the decimals needed to read them.
    pub amount: String,
    pub amount_decimals: u8,
    pub slippage_bps: u16,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StrategyView {
    pub id: String,
    pub venue: String,
    pub chain: String,
    pub base_token: String,
    pub quote_token: String,
}

/// Operator-facing order without the stored raw quote.
#[derive(Debug, Serialize, JsonSchema)]
pub struct OrderView {
    pub id: String,
    pub level_id: String,
    pub status: String,
    pub phase: String,
    pub chain: String,
    pub token_in: String,
    pub token_out: String,
    pub tx_hash: Option<String>,
    pub swap_attempts: u32,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct DashboardSnapshot {
    pub version: String,
    pub started_at: i64,
    pub generated_at: i64,
    pub allow_broadcast: bool,
    pub strategies: Vec<StrategyView>,
    pub levels: Vec<DashboardLevelView>,
    pub orders: Vec<OrderView>,
    pub feeds: Vec<FeedSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct DashboardLevelView {
    pub id: String,
    pub strategy_id: String,
    pub side: String,
    pub token_in: String,
    pub token_out: String,
    pub trigger_price_usd: f64,
    pub price_pair: PricePair,
    pub amount: String,
    pub amount_decimals: u8,
    pub slippage_bps: u16,
    pub runtime_state: RuntimeLevelState,
}

/// Object wrappers required by MCP output schemas.
#[derive(Debug, Serialize, JsonSchema)]
pub struct LevelList {
    pub levels: Vec<LevelView>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StrategyList {
    pub strategies: Vec<StrategyView>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct OrderList {
    pub orders: Vec<OrderView>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OrdersRequest {
    /// How many of the most recent orders to return.
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LevelId {
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Deleted {
    pub id: String,
    pub deleted: bool,
}

fn to_mcp(error: anyhow::Error) -> McpError {
    McpError::invalid_params(error.to_string(), None)
}

fn strategy_view(strategy: &Strategy) -> StrategyView {
    StrategyView {
        id: strategy.id.clone(),
        venue: strategy.venue.as_str().to_string(),
        chain: strategy.chain.clone(),
        base_token: strategy.base_token.clone(),
        quote_token: strategy.quote_token.clone(),
    }
}

fn level_view(entry: &StrategyLevel) -> LevelView {
    let direction = trade_direction(&entry.strategy, entry.level.side);
    LevelView {
        id: entry.level.id.clone(),
        strategy_id: entry.strategy.id.clone(),
        venue: entry.strategy.venue.as_str().to_string(),
        chain: entry.strategy.chain.clone(),
        side: entry.level.side.as_str().to_string(),
        token_in: direction.token_in.to_owned(),
        token_out: direction.token_out.to_owned(),
        trigger_price_usd: entry.level.trigger_price_usd,
        amount: entry.level.amount.to_string(),
        amount_decimals: entry.level.amount_decimals,
        slippage_bps: entry.level.slippage_bps,
    }
}

fn order_view(order: &Order) -> OrderView {
    OrderView {
        id: order.id.clone(),
        level_id: order.level_id.clone(),
        status: order.status().as_str().to_string(),
        phase: phase(order).to_string(),
        chain: order.chain.clone(),
        token_in: order.token_in.clone(),
        token_out: order.token_out.clone(),
        tx_hash: order.tx_hash().map(str::to_string),
        swap_attempts: order.swap_attempts,
        created_at: order.created_at,
    }
}

fn dashboard_level_view(
    entry: &StrategyLevel,
    runtime_state: RuntimeLevelState,
    resolver: &TokenResolver,
) -> anyhow::Result<DashboardLevelView> {
    let direction = trade_direction(&entry.strategy, entry.level.side);
    let price_pair = resolver.price_pair(&entry.strategy).with_context(|| {
        format!(
            "strategy {} has no configured price pair",
            entry.strategy.id
        )
    })?;
    Ok(DashboardLevelView {
        id: entry.level.id.clone(),
        strategy_id: entry.strategy.id.clone(),
        side: entry.level.side.as_str().to_string(),
        token_in: direction.token_in.to_owned(),
        token_out: direction.token_out.to_owned(),
        trigger_price_usd: entry.level.trigger_price_usd,
        price_pair,
        amount: entry.level.amount.to_string(),
        amount_decimals: entry.level.amount_decimals,
        slippage_bps: entry.level.slippage_bps,
        runtime_state,
    })
}

// Expose the exact phase for debugging stuck orders.
fn phase(order: &Order) -> &'static str {
    use tempo_agentic_strategy::OrderState as S;
    match &order.state {
        S::Withdrawing { .. } => "withdrawing",
        S::SwapReady { .. } => "swap_ready",
        S::Broadcasting { .. } => "broadcasting",
        S::Submitted { .. } => "submitted",
        S::Depositing { .. } => "depositing",
        S::Filled { .. } => "filled",
        S::Failed { .. } => "failed",
        S::SwapQuarantined { .. } => "quarantined",
    }
}

#[tool_router]
impl AdminHandler {
    #[tool(description = "List every configured strategy market.")]
    async fn strategies(&self) -> Result<Json<StrategyList>, McpError> {
        let strategies = self.strategies.list_strategies().await.map_err(to_mcp)?;
        Ok(Json(StrategyList {
            strategies: strategies.iter().map(strategy_view).collect(),
        }))
    }

    #[tool(
        description = "Store a strategy market. An existing market can change only before its first level is added."
    )]
    async fn set_strategy(
        &self,
        Parameters(draft): Parameters<StrategyDraft>,
    ) -> Result<Json<StrategyView>, McpError> {
        let strategy = validate_strategy(self.tokens.as_ref(), self.prices.as_ref(), &draft)
            .map_err(to_mcp)?;
        self.strategies
            .upsert_strategy(&strategy)
            .await
            .map_err(to_mcp)?;
        Ok(Json(strategy_view(&strategy)))
    }

    #[tool(
        description = "Report what the running daemon is doing: version, whether broadcasting is allowed, how many rules are stored, and how many orders sit in each status. Check allow_broadcast before telling anyone a rule will trade for real."
    )]
    async fn status(&self) -> Result<Json<DaemonStatus>, McpError> {
        let levels = self.levels.list_levels().await.map_err(to_mcp)?;
        let orders = self.orders.list_orders().await.map_err(to_mcp)?;
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for order in &orders {
            *counts
                .entry(order.status().as_str().to_string())
                .or_default() += 1;
        }
        Ok(Json(DaemonStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            allow_broadcast: self.dashboard.runtime.snapshot().allow_broadcast,
            levels: levels.len(),
            orders: counts,
        }))
    }

    #[tool(description = "List every standing rule the daemon watches prices against.")]
    async fn levels(&self) -> Result<Json<LevelList>, McpError> {
        let levels = self.levels.list_levels().await.map_err(to_mcp)?;
        Ok(Json(LevelList {
            levels: levels.iter().map(level_view).collect(),
        }))
    }

    #[tool(
        description = "List recent execution attempts, newest first. Each order belongs to the rule named by level_id."
    )]
    async fn orders(
        &self,
        Parameters(request): Parameters<OrdersRequest>,
    ) -> Result<Json<OrderList>, McpError> {
        let orders = self.orders.list_orders().await.map_err(to_mcp)?;
        // Stored oldest first, and the interesting end is the recent one.
        Ok(Json(OrderList {
            orders: orders
                .iter()
                .rev()
                .take(request.limit.unwrap_or(DEFAULT_ORDER_LIMIT))
                .map(order_view)
                .collect(),
        }))
    }

    #[tool(
        description = "Store a level for an existing strategy, replacing any level with the same id. Buy spends the strategy's quote token for its base token; sell spends base for quote. The amount is in whole units of that input token. A stored level can spend funds once its price is crossed, so show it to the user before calling this."
    )]
    async fn set_level(
        &self,
        Parameters(draft): Parameters<LevelDraft>,
    ) -> Result<Json<LevelView>, McpError> {
        let strategy = self
            .strategies
            .get_strategy(&draft.strategy_id)
            .await
            .map_err(to_mcp)?
            .ok_or_else(|| McpError::invalid_params("strategy does not exist", None))?;
        let level = validate_level(
            self.tokens.as_ref(),
            self.max_slippage_bps,
            self.prices.as_ref(),
            &strategy,
            &draft,
        )
        .map_err(to_mcp)?;
        self.levels
            .upsert_level(&level, &strategy)
            .await
            .map_err(to_mcp)?;
        Ok(Json(level_view(&StrategyLevel { strategy, level })))
    }

    #[tool(
        description = "Delete a standing rule by id. Orders it already started are kept as the record of what the daemon did."
    )]
    async fn delete_level(
        &self,
        Parameters(request): Parameters<LevelId>,
    ) -> Result<Json<Deleted>, McpError> {
        self.levels
            .delete_level(&request.id)
            .await
            .map_err(to_mcp)?;
        Ok(Json(Deleted {
            id: request.id,
            deleted: true,
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AdminHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "tempo-agentic-daemon",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "This is a running tempo-agentic daemon. Strategies define markets and levels \
                 spend funds once their price is crossed, so read status first, show changes to \
                 the user in full, and only call set_strategy or set_level after an explicit yes. Orders are the \
                 record of what already happened and cannot be edited from here.",
            )
    }
}
