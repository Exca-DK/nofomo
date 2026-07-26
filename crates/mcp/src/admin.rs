use std::collections::BTreeMap;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ErrorData as McpError, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{Json, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tempo_agentic_config::EvmConfig;
use tempo_agentic_price::PriceSource;
use tempo_agentic_strategy::{Level, LevelStore, Order, OrderStore};
use tempo_agentic_trigger::{LevelDraft, validate_level};

/// Orders returned when the caller does not say how many it wants.
const DEFAULT_ORDER_LIMIT: usize = 50;

/// Model Context Protocol handler for inspecting and steering a running daemon.
///
/// Reads and writes the same database the daemon works from, so everything here
/// is about the live process rather than a private copy.
#[derive(Clone)]
pub struct AdminHandler {
    levels: Arc<dyn LevelStore>,
    orders: Arc<dyn OrderStore>,
    evm: EvmConfig,
    max_slippage_bps: u16,
    allow_broadcast: bool,
    /// Consulted before a rule is stored, so one that nothing could price is
    /// refused rather than armed and silent.
    prices: Arc<dyn PriceSource>,
    tool_router: ToolRouter<Self>,
}

impl AdminHandler {
    pub fn new(
        levels: Arc<dyn LevelStore>,
        orders: Arc<dyn OrderStore>,
        evm: EvmConfig,
        max_slippage_bps: u16,
        allow_broadcast: bool,
        prices: Arc<dyn PriceSource>,
    ) -> Self {
        Self {
            levels,
            orders,
            evm,
            max_slippage_bps,
            allow_broadcast,
            prices,
            tool_router: Self::tool_router(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DaemonStatus {
    pub version: String,
    /// False means the daemon quotes, builds and signs but never sends. An agent
    /// has to know this before it claims a rule will trade.
    pub allow_broadcast: bool,
    pub levels: usize,
    /// Order counts by status: pending, submitted, filled, failed, quarantined.
    pub orders: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LevelView {
    pub id: String,
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

/// An order as an operator needs to see it.
///
/// Deliberately not the stored [`Order`]: that carries the venue's raw quote
/// blob, which is noise to a reader and can run to kilobytes.
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

/// MCP requires a tool's output schema to be rooted at an object, so the lists
/// travel wrapped rather than bare.
#[derive(Debug, Serialize, JsonSchema)]
pub struct LevelList {
    pub levels: Vec<LevelView>,
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

fn level_view(level: &Level) -> LevelView {
    LevelView {
        id: level.id.clone(),
        venue: level.venue.as_str().to_string(),
        chain: level.chain.clone(),
        side: level.side.as_str().to_string(),
        token_in: level.token_in.clone(),
        token_out: level.token_out.clone(),
        trigger_price_usd: level.trigger_price_usd,
        amount: level.amount.to_string(),
        amount_decimals: level.amount_decimals,
        slippage_bps: level.slippage_bps,
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

// Finer than `status`: an operator debugging a stuck order needs to know whether
// it is waiting to be signed or waiting for a receipt.
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
            allow_broadcast: self.allow_broadcast,
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
        description = "Store a standing rule, replacing any rule with the same id. The chain and both tokens must be configured, the amount is in whole units of token_in, and slippage_bps must not exceed the configured maximum. A stored rule will spend funds once its price is crossed, so show it to the user before calling this."
    )]
    async fn set_level(
        &self,
        Parameters(draft): Parameters<LevelDraft>,
    ) -> Result<Json<LevelView>, McpError> {
        let level = validate_level(
            &self.evm,
            self.max_slippage_bps,
            self.prices.as_ref(),
            &draft,
        )
        .map_err(to_mcp)?;
        self.levels.upsert_level(&level).await.map_err(to_mcp)?;
        Ok(Json(level_view(&level)))
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
                "This is a running tempo-agentic daemon. Rules stored here spend real funds \
                 once their price is crossed, so read status first, show a rule to the user \
                 in full, and only call set_level after an explicit yes. Orders are the \
                 record of what already happened and cannot be edited from here.",
            )
    }
}
