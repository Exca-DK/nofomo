use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use tempo_agentic_domain::{ChainClient, ExecutionPlan, ReceiptStatus, Signer, TradeVenue};
use tempo_agentic_strategy::{Order, OrderState};

use crate::machine::{Action, Outcome, RECEIPT_DEADLINE_SECS};

/// Everything the execution loop needs to reach the outside world.
pub struct ExecDeps {
    pub venues: Vec<Arc<dyn TradeVenue>>,
    pub chains: HashMap<u64, Arc<dyn ChainClient>>,
    pub signer: Arc<dyn Signer>,
    /// Whether signed transactions may be sent.
    pub allow_broadcast: bool,
}

impl ExecDeps {
    fn venue(&self, name: &str) -> Result<&Arc<dyn TradeVenue>> {
        self.venues
            .iter()
            .find(|venue| venue.name() == name)
            .with_context(|| format!("unsupported trade venue {name}"))
    }

    fn chain(&self, plan: &ExecutionPlan) -> Result<&Arc<dyn ChainClient>> {
        let ExecutionPlan::Uniswap { chain_id, .. } = plan else {
            bail!("only Uniswap execution plans can be executed");
        };
        self.chains
            .get(chain_id)
            .with_context(|| format!("no chain client configured for chain {chain_id}"))
    }
}

/// Runs an action and returns every failure as a recordable outcome.
pub async fn perform(deps: &ExecDeps, order: &Order, action: Action) -> Outcome {
    match action {
        Action::Sign => match sign(deps, order).await {
            Ok(outcome) => outcome,
            Err(error) => failed(error),
        },
        Action::Broadcast { signed } if !deps.allow_broadcast => {
            tracing::warn!(
                order = %order.id,
                tx_hash = %signed.hash,
                "broadcast blocked; the transaction is signed but stays here"
            );
            Outcome::BroadcastBlocked
        }
        Action::Broadcast { signed } => match deps.chain(&order.plan) {
            Err(error) => failed(error),
            Ok(chain) => match chain.broadcast(&signed).await {
                Ok(tx_hash) => Outcome::Broadcast {
                    tx_hash,
                    at: now_unix(),
                },
                Err(error) => failed(error),
            },
        },
        Action::CheckReceipt { tx_hash } => check_receipt(deps, order, &tx_hash).await,
        // Unreachable: the loop stops on Done before it gets here.
        Action::Done => Outcome::StillPending,
    }
}

async fn sign(deps: &ExecDeps, order: &Order) -> Result<Outcome> {
    let venue = deps.venue(order.venue.as_str())?;
    let chain = deps.chain(&order.plan)?;

    // Re-derive remaining steps after each confirmed approval.
    let step = *venue
        .steps(&order.plan)
        .await
        .context("read the plan's remaining steps")?
        .first()
        .context("venue reports no remaining steps for an unfinished plan")?;

    // Pending nonces stay unique within one sweep.
    let ctx = chain.tx_context(deps.signer.address()).await?;
    let unsigned = venue.build(&order.plan, step, &ctx).await?;
    let signed = deps.signer.sign(&unsigned).await?;
    Ok(Outcome::Signed { step, signed })
}

// Receipt errors wait because the transaction may be on chain.
async fn check_receipt(deps: &ExecDeps, order: &Order, tx_hash: &str) -> Outcome {
    let chain = match deps.chain(&order.plan) {
        Ok(chain) => chain,
        Err(error) => {
            tracing::warn!(order = %order.id, %error, "no chain client for the receipt check");
            return Outcome::StillPending;
        }
    };
    match chain.confirmation(tx_hash).await {
        Ok(ReceiptStatus::Success) => Outcome::Confirmed,
        Ok(ReceiptStatus::Reverted) => Outcome::Reverted,
        Ok(ReceiptStatus::Pending) if past_deadline(order) => {
            tracing::warn!(
                order = %order.id,
                tx_hash,
                "no receipt within the deadline; giving up on this transaction"
            );
            Outcome::ReceiptTimedOut
        }
        Ok(ReceiptStatus::Pending) => Outcome::StillPending,
        Err(error) => {
            tracing::warn!(order = %order.id, tx_hash, %error, "receipt check failed");
            Outcome::StillPending
        }
    }
}

// A deadline releases orders whose receipt will never arrive.
fn past_deadline(order: &Order) -> bool {
    match &order.state {
        OrderState::Submitted { submitted_at, .. } => {
            now_unix() > submitted_at + RECEIPT_DEADLINE_SECS
        }
        _ => false,
    }
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

// Preserve the full error chain in durable state.
fn failed(error: anyhow::Error) -> Outcome {
    Outcome::ExecFailed {
        reason: format!("{error:#}"),
    }
}
