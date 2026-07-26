use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tempo_agentic_domain::{ChainClient, ExecutionPlan, ReceiptStatus, Signer, TradeVenue};
use tempo_agentic_strategy::Order;

use crate::machine::{Action, Outcome};

/// Everything the execution loop needs to reach the outside world.
pub struct ExecDeps {
    pub venues: Vec<Arc<dyn TradeVenue>>,
    pub chains: HashMap<u64, Arc<dyn ChainClient>>,
    pub signer: Arc<dyn Signer>,
    /// Whether signed transactions may actually be sent. Off means every other
    /// step still runs, so a blocked process exercises the whole path but spends
    /// nothing.
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

/// Runs one action and reports what came of it.
///
/// Never returns an error: a failure is an outcome the state machine has to
/// record, not something the caller could retry differently.
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
                Ok(tx_hash) => Outcome::Broadcast { tx_hash },
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

    // Asked fresh every time instead of read from the state: once an approval
    // confirms, the venue stops asking for it, so the sequence repairs itself.
    let step = *venue
        .steps(&order.plan)
        .await
        .context("read the plan's remaining steps")?
        .first()
        .context("venue reports no remaining steps for an unfinished plan")?;

    // Reading the nonce off the latest block is safe here because the previous
    // step's receipt already exists, so the node counts that transaction.
    let ctx = chain.tx_context(deps.signer.address()).await?;
    let unsigned = venue.build(&order.plan, step, &ctx).await?;
    let signed = deps.signer.sign(&unsigned).await?;
    Ok(Outcome::Signed { step, signed })
}

// An unreadable receipt is not a failed trade. Calling it one would abandon a
// transaction that may well be on chain, so every error here waits instead.
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
        Ok(ReceiptStatus::Pending) => Outcome::StillPending,
        Err(error) => {
            tracing::warn!(order = %order.id, tx_hash, %error, "receipt check failed");
            Outcome::StillPending
        }
    }
}

// The alternate form keeps the whole `anyhow` context chain, which is the only
// record of why an order failed once it is in the database.
fn failed(error: anyhow::Error) -> Outcome {
    Outcome::ExecFailed {
        reason: format!("{error:#}"),
    }
}
