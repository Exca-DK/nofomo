use anyhow::{Context, Result};
use tempo_agentic_strategy::OrderStore;

use crate::{Outcome, apply};

/// Releases a quarantined order and reports the level it frees.
pub async fn resolve_quarantine(orders: &dyn OrderStore, order_id: &str) -> Result<String> {
    let mut order = orders
        .get_order(order_id)
        .await?
        .with_context(|| format!("no order {order_id}"))?;
    // `failed` re-arms the level with a fresh quote.
    let released = apply(&order, Outcome::QuarantineResolved)
        .with_context(|| format!("order {order_id} is not quarantined"))?
        .context("releasing a quarantine has to change the state")?;
    order.state = released;
    order.swap_attempts = 0;
    order.swap_retry_after_ts = None;
    orders.upsert_order(&order).await?;
    Ok(order.level_id)
}
