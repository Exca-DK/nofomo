use anyhow::Result;
use async_trait::async_trait;

use crate::{
    ExecuteTradeRequest, ExecutionView, MarketResearch, MarketResearchRequest, QuoteTradeRequest,
    QuoteView, SignedTx, UnsignedTx,
};

/// Signing port. Key material never leaves the implementation.
#[async_trait]
pub trait Signer: Send + Sync {
    /// The address this signer controls, 0x-prefixed.
    fn address(&self) -> &str;

    /// Signs a valid transaction offline and returns its future hash.
    async fn sign(&self, tx: &UnsignedTx) -> Result<SignedTx>;
}

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

    /// Consumes a quote and creates an attempt atomically, even if rejected.
    async fn claim_quote(&self, request: &ExecuteTradeRequest, now: u64) -> Result<i64>;

    async fn record_execution_success(&self, attempt_id: i64, result: &ExecutionView)
    -> Result<()>;

    async fn record_execution_failure(&self, attempt_id: i64) -> Result<()>;
}
