use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tempo_agentic_domain::{
    ChainClient, ChainId, ExecutionPlan, ReceiptStatus, SignedEvmTx, SignedSuiTx, SignedTx, Signer,
    TradeVenue,
};
use tempo_agentic_strategy::{Order, OrderState};

use crate::machine::{Action, Outcome, RECEIPT_DEADLINE_SECS};

/// Everything the execution loop needs to reach the outside world.
pub struct ExecDeps {
    pub venues: Vec<Arc<dyn TradeVenue>>,
    pub chains: HashMap<ChainId, Arc<dyn ChainClient>>,
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
        let chain = plan.chain();
        self.chains
            .get(&chain)
            .with_context(|| format!("no chain client configured for {chain}"))
    }
}

/// Runs an action and returns every failure as a recordable outcome.
pub async fn perform(deps: &ExecDeps, order: &Order, action: Action) -> Outcome {
    match action {
        Action::Sign => match sign(deps, order).await {
            Ok(outcome) => outcome,
            Err(error) => failed(error),
        },
        Action::Broadcast { tx_hash, .. } if !deps.allow_broadcast => {
            tracing::warn!(
                order = %order.id,
                tx_hash,
                "broadcast blocked; the transaction is signed but stays here"
            );
            Outcome::BroadcastBlocked
        }
        Action::Broadcast { signed_tx, tx_hash } => {
            match broadcast(deps, order, &signed_tx, &tx_hash).await {
                Ok(outcome) => outcome,
                Err(error) => failed(error),
            }
        }
        Action::CheckReceipt { tx_hash } => check_receipt(deps, order, &tx_hash).await,
        // Unreachable: the loop stops on Done before it gets here.
        Action::Done => Outcome::StillPending,
    }
}

async fn sign(deps: &ExecDeps, order: &Order) -> Result<Outcome> {
    let venue = deps.venue(order.venue.as_str())?;
    let chain = deps.chain(&order.plan)?;
    let family = order.plan.chain().family();

    // Re-derive remaining steps after each confirmed approval.
    let step = *venue
        .steps(&order.plan)
        .await
        .context("read the plan's remaining steps")?
        .first()
        .context("venue reports no remaining steps for an unfinished plan")?;

    // Pending nonces stay unique within one sweep.
    let ctx = chain.tx_context(deps.signer.address(family)?).await?;
    let unsigned = venue.build(&order.plan, step, &ctx).await?;
    let signed = deps.signer.sign(&unsigned).await?;

    Ok(Outcome::Signed {
        step,
        signed_tx: signed.to_wire()?,
        tx_hash: signed.hash(),
    })
}

async fn broadcast(
    deps: &ExecDeps,
    order: &Order,
    signed_tx: &str,
    tx_hash: &str,
) -> Result<Outcome> {
    let signed = restore_signed(&order.plan, signed_tx, tx_hash)?;
    let tx_hash = deps.chain(&order.plan)?.broadcast(&signed).await?;
    Ok(Outcome::Broadcast {
        tx_hash,
        at: now_unix(),
    })
}

// Decode according to the plan's chain family.
fn restore_signed(plan: &ExecutionPlan, signed_tx: &str, tx_hash: &str) -> Result<SignedTx> {
    match plan.chain() {
        ChainId::Evm(_) => Ok(SignedTx::Evm(SignedEvmTx {
            raw: signed_tx.to_string(),
            hash: tx_hash.to_string(),
        })),
        ChainId::Sui => Ok(SignedTx::Sui(Box::new(SignedSuiTx::from_wire(signed_tx)?))),
    }
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
