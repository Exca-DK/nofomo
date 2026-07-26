use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tempo_agentic_strategy::{Order, OrderState, OrderStore};
use tokio::sync::Notify;

use crate::io::{ExecDeps, now_unix, perform};
use crate::machine::{Action, Outcome, apply, next_action, swap_retry_backoff_secs};

/// Transition cap preventing a bad venue from spinning.
const MAX_TRANSITIONS_PER_PASS: usize = 12;

/// Wakes the execution loop.
#[derive(Default)]
pub struct Waker {
    notify: Arc<Notify>,
}

impl Waker {
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// Returns a wake-only handle for work producers.
    pub fn notifier(&self) -> Arc<Notify> {
        self.notify.clone()
    }

    /// Waits for a cached wake or timeout.
    pub async fn wait(&self, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, self.notify.notified()).await;
    }
}

/// Drives open orders on each wake or poll until dropped.
pub async fn run(
    deps: Arc<ExecDeps>,
    orders: Arc<dyn OrderStore>,
    waker: Arc<Waker>,
    poll: Duration,
) {
    loop {
        if let Err(error) = sweep(&deps, orders.as_ref()).await {
            tracing::warn!(%error, "sweep failed");
        }
        waker.wait(poll).await;
    }
}

/// Advances every open order; individual failures do not stop the pass.
pub async fn sweep(deps: &ExecDeps, orders: &dyn OrderStore) -> Result<()> {
    let open = orders
        .list_orders()
        .await
        .context("list orders")?
        .into_iter()
        .filter(|order| !order.is_terminal());
    for mut order in open {
        if let Err(error) = drive_order(deps, orders, &mut order).await {
            tracing::warn!(order = %order.id, %error, "failed to drive order");
        }
    }
    Ok(())
}

/// Advances one order, persisting each state before its side effect.
pub async fn drive_order(
    deps: &ExecDeps,
    orders: &dyn OrderStore,
    order: &mut Order,
) -> Result<()> {
    for _ in 0..MAX_TRANSITIONS_PER_PASS {
        // Honor retry backoff before any work.
        if order.swap_retry_after_ts.is_some_and(|at| now_unix() < at) {
            return Ok(());
        }

        let action = next_action(order);
        if action == Action::Done {
            return Ok(());
        }
        let outcome = perform(deps, order, action).await;
        let still_pending = outcome == Outcome::StillPending;
        match apply(order, outcome) {
            Ok(Some(next)) => {
                order.state = next;
                // Clear the completed attempt's retry.
                order.swap_retry_after_ts = None;
                orders
                    .upsert_order(order)
                    .await
                    .context("persist transition")?;
                announce(order);
            }
            // Back off before retrying an unchanged state.
            Ok(None) if !still_pending => {
                order.swap_attempts = order.swap_attempts.saturating_add(1);
                let backoff = swap_retry_backoff_secs(order.swap_attempts);
                order.swap_retry_after_ts = Some(now_unix() + backoff);
                orders
                    .upsert_order(order)
                    .await
                    .context("persist retry schedule")?;
                tracing::warn!(
                    order = %order.id,
                    attempts = order.swap_attempts,
                    backoff,
                    "broadcast failed; resending the same bytes later"
                );
                return Ok(());
            }
            Ok(None) => {}
            // Do not turn invalid state into a trade failure.
            Err(error) => {
                tracing::error!(order = %order.id, %error, "invalid transition");
                return Ok(());
            }
        }
        if still_pending {
            return Ok(());
        }
    }
    tracing::warn!(
        order = %order.id,
        "stopped after {MAX_TRANSITIONS_PER_PASS} transitions in one pass"
    );
    Ok(())
}

fn announce(order: &Order) {
    match &order.state {
        OrderState::Failed { reason, .. } => {
            tracing::warn!(order = %order.id, reason, "order failed");
        }
        // Park before the order becomes operator-only.
        OrderState::SwapQuarantined {
            reason, tx_hash, ..
        } => {
            tracing::warn!(
                order = %order.id,
                level = %order.level_id,
                attempts = order.swap_attempts,
                tx_hash = tx_hash.as_deref().unwrap_or("none"),
                reason,
                "order quarantined after exhausting its broadcast retries; the \
                 level stays blocked until resolve-quarantine runs"
            );
        }
        state => {
            tracing::info!(
                order = %order.id,
                status = order.status().as_str(),
                phase = ?state,
                "order advanced"
            );
        }
    }
}
