mod common;

use std::time::Duration;

use common::{Harness, Script, order, phase};
use tempo_agentic_orchestrator::Waker;
use tempo_agentic_strategy::OrderState;

#[tokio::test]
async fn one_failing_order_does_not_stop_the_others() {
    let harness = Harness::new("sweep", Script::default()).await;
    // Chain 1 has no client configured, so this one cannot even be signed. The
    // id sorts ahead of `o-1`, which is the order `list_orders` walks in, so the
    // sweep meets the broken order first and has to carry on past it.
    harness.put(&order("o-0-stranded", 1)).await;

    harness.sweep().await;

    assert_eq!(phase(&harness.stored("o-0-stranded").await.state), "failed");
    assert_eq!(phase(&harness.stored("o-1").await.state), "filled");
    harness.cleanup();
}

#[tokio::test]
async fn a_finished_order_is_never_touched_again() {
    let harness = Harness::new("terminal", Script::default()).await;
    let mut done = harness.stored("o-1").await;
    done.state = OrderState::Filled {
        tx_hash: "0xold".into(),
    };
    harness.put(&done).await;

    harness.sweep().await;

    assert_eq!(harness.steps_calls(), 0);
    assert!(harness.seen().is_empty());
    harness.cleanup();
}

// The producer wakes the loop the moment it stores an order, which can easily be
// before the loop comes back round to wait.
#[tokio::test]
async fn waking_before_waiting_is_not_lost() {
    let waker = Waker::default();
    waker.wake();

    tokio::time::timeout(
        Duration::from_millis(50),
        waker.wait(Duration::from_secs(30)),
    )
    .await
    .expect("a wake that arrived before the wait must still release it");
}
