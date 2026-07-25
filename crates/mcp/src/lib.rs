use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ErrorData as McpError, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{Json, ServerHandler, tool, tool_handler, tool_router};

use tempo_agentic_agent::AgentService;
use tempo_agentic_domain::{
    ExecuteTradeRequest, ExecutionView, MarketResearch, MarketResearchRequest, QuoteTradeRequest,
    QuoteView,
};

/// Model Context Protocol server handler for agent trading capabilities.
#[derive(Clone)]
pub struct AgentHandler {
    service: AgentService,
    tool_router: ToolRouter<Self>,
}

impl AgentHandler {
    pub fn new(service: AgentService) -> Self {
        Self {
            service,
            tool_router: Self::tool_router(),
        }
    }
}

fn to_mcp(error: anyhow::Error) -> McpError {
    McpError::invalid_params(error.to_string(), None)
}

#[tool_router]
impl AgentHandler {
    #[tool(
        description = "Research a token pair through live Uniswap subgraphs on The Graph. Use this before proposing an EVM trade."
    )]
    async fn market_research(
        &self,
        Parameters(request): Parameters<MarketResearchRequest>,
    ) -> Result<Json<MarketResearch>, McpError> {
        self.service
            .market_research(request)
            .await
            .map(Json)
            .map_err(to_mcp)
    }

    #[tool(
        description = "Create a short-lived, one-time exact-input quote. Uniswap checks all requested pre-funded chains without bridging; DeepBook dry-runs the configured Sui testnet hBTC pool. This never executes a transaction."
    )]
    async fn quote_trade(
        &self,
        Parameters(request): Parameters<QuoteTradeRequest>,
    ) -> Result<Json<QuoteView>, McpError> {
        self.service
            .quote_trade(request)
            .await
            .map(Json)
            .map_err(to_mcp)
    }

    #[tool(
        description = "Execute exactly one stored quote after explicit user confirmation. confirmed must be true; the quote is consumed on the first attempt."
    )]
    async fn execute_trade(
        &self,
        Parameters(request): Parameters<ExecuteTradeRequest>,
    ) -> Result<Json<ExecutionView>, McpError> {
        self.service
            .execute_trade(request)
            .await
            .map(Json)
            .map_err(to_mcp)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AgentHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("tempo-agentic", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "tempo-agentic is a user-controlled trading agent. Research with The Graph, then quote, \
             show the full quote to the user, and only call execute_trade after an explicit yes. \
             Never invent a quote ID and never claim that Uniswap bridges funds.",
            )
    }
}
