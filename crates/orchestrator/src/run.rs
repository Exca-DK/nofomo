use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tempo_agentic_strategy::{Order, OrderState, OrderStore};
use tokio::sync::Notify;

use crate::io::{ExecDeps, perform};
use crate::machine::{Action, Outcome, apply, next_action, swap_retry_backoff_secs};

/// Transitions one pass may make before yielding. A venue that keeps asking for
/// the same approval would otherwise spin here, broadcasting every round.
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

    /// The waking half on its own, for a producer that should not be able to
    /// wait. The trigger takes this: it creates work and says so, but the loop
    /// that consumes the work is the only one allowed to sleep on it.
    pub fn notifier(&self) -> Arc<Notify> {
        self.notify.clone()
    }

    /// Waits until woken or the timeout expires. One permit is cached, so waking
    /// before the wait begins is not lost.
    pub async fn wait(&self, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, self.notify.notified()).await;
    }
}

/// Drives open orders forward on every wake, and at least once per `poll`.
///
/// Runs until the task is dropped.
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

/// One pass over every order that is not finished.
///
/// Returns an error only when the orders cannot be read at all; one order
/// failing never stops the others.
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

/// Advances one order as far as it goes right now, saving after every transition.
///
/// The new state is written before the action it enables runs, so a crash leaves
/// behind a row that says what was already attempted.
pub async fn drive_order(
    deps: &ExecDeps,
    orders: &dyn OrderStore,
    order: &mut Order,
) -> Result<()> {
    for _ in 0..MAX_TRANSITIONS_PER_PASS {
        // Checked before anything else. A broadcast retry deliberately leaves the
        // state alone, so without this gate the loop would resend on the spot.
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
                // Whatever retry was scheduled belonged to the attempt that just
                // ended.
                order.swap_retry_after_ts = None;
                orders
                    .upsert_order(order)
                    .await
                    .context("persist transition")?;
                announce(order);
            }
            // The state held but the attempt failed, which happens only where a
            // resend is worth trying. Sleep on it rather than hammer the node.
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
            // A data or programming error rather than a trade failure, so stop
            // here instead of marking an order failed for something it did not do.
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
        // The last moment a sweep can still see this order: it turns terminal
        // next, its level stays blocked, and only an operator can release it.
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

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}
