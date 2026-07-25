use anyhow::Result;
use async_trait::async_trait;

use crate::{
    ExecuteTradeRequest, ExecutionView, MarketResearch, MarketResearchRequest, QuoteTradeRequest,
    QuoteView,
};

/// Storage port for recording market research, quotes, and execution attempts.
#[async_trait]
pub trait AuditStore: Send + Sync {
    fn session_id(&self) -> &str;

    async fn record_research(
        &self,
        request: &MarketResearchRequest,
        result: &MarketResearch,
    ) -> Result<()>;

    async fn record_quote(
        &self,
        request: &QuoteTradeRequest,
        quote: &QuoteView,
        plan_digest: &str,
    ) -> Result<()>;

    /// Atomically consumes a quote and creates an execution attempt.
    /// The quote is consumed even when confirmation is false or it has expired.
    async fn claim_quote(&self, request: &ExecuteTradeRequest, now: u64) -> Result<i64>;

    async fn record_execution_success(&self, attempt_id: i64, result: &ExecutionView)
    -> Result<()>;

    async fn record_execution_failure(&self, attempt_id: i64) -> Result<()>;
}
