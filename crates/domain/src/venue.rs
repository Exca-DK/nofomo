use anyhow::Result;
use async_trait::async_trait;

use crate::{ExecutionPlan, QuoteDraft, QuoteTradeRequest, TransactionReference};

/// Execution result containing transactions generated for a target trading venue.
#[derive(Debug)]
pub struct VenueExecution {
    pub venue: String,
    pub chain: String,
    pub transactions: Vec<TransactionReference>,
}

/// Port interface for trading venues that produce quotes and execute trades.
#[async_trait]
pub trait TradeVenue: Send + Sync {
    fn name(&self) -> &'static str;
    async fn quote(&self, request: &QuoteTradeRequest) -> Result<QuoteDraft>;
    async fn execute(&self, plan: &ExecutionPlan) -> Result<VenueExecution>;
}
