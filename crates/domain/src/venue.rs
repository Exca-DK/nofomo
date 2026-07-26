use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{ExecutionPlan, QuoteDraft, QuoteTradeRequest, TxContext, UnsignedTx};

/// One transaction in a plan: allowance reset, approval, or swap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecStep {
    Cancel,
    Approval,
    Swap,
}

impl ExecStep {
    /// Label stored with the transaction.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Approval => "approval",
            Self::Swap => "swap",
        }
    }
}

/// Stateless trading venue that quotes and builds restart-safe transactions.
#[async_trait]
pub trait TradeVenue: Send + Sync {
    fn name(&self) -> &'static str;

    async fn quote(&self, request: &QuoteTradeRequest) -> Result<QuoteDraft>;

    /// The steps this plan still requires, in execution order.
    async fn steps(&self, plan: &ExecutionPlan) -> Result<Vec<ExecStep>>;

    /// Builds and validates one unsigned transaction.
    async fn build(
        &self,
        plan: &ExecutionPlan,
        step: ExecStep,
        ctx: &TxContext,
    ) -> Result<UnsignedTx>;

    /// Executes the plan monolithically. Used by non-EVM venues.
    async fn execute(&self, _plan: &ExecutionPlan) -> Result<Vec<crate::TransactionReference>> {
        anyhow::bail!("monolithic execution is not supported by this venue");
    }
}
