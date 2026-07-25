use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{ExecutionPlan, QuoteDraft, QuoteTradeRequest, TxContext, UnsignedTx};

/// One transaction in a plan's execution sequence.
///
/// A swap on an ERC-20 input may first need the existing allowance zeroed
/// (`Cancel`) and then raised (`Approval`), so a plan can span three
/// transactions that each have to be tracked separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecStep {
    Cancel,
    Approval,
    Swap,
}

impl ExecStep {
    /// Label recorded on a [`TransactionReference`].
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Approval => "approval",
            Self::Swap => "swap",
        }
    }
}

/// Port interface for trading venues that produce quotes and build transactions.
///
/// `steps` and `build` are deliberately stateless: after a restart, `build` is
/// called with only the plan and the step, so it must re-derive everything it
/// needs rather than rely on an earlier `steps` call.
#[async_trait]
pub trait TradeVenue: Send + Sync {
    fn name(&self) -> &'static str;

    async fn quote(&self, request: &QuoteTradeRequest) -> Result<QuoteDraft>;

    /// The steps this plan still requires, in execution order.
    async fn steps(&self, plan: &ExecutionPlan) -> Result<Vec<ExecStep>>;

    /// Builds the unsigned transaction for one step.
    ///
    /// Returns an error if the plan targets another venue or the venue's API
    /// returns a transaction that fails validation.
    async fn build(
        &self,
        plan: &ExecutionPlan,
        step: ExecStep,
        ctx: &TxContext,
    ) -> Result<UnsignedTx>;
}
