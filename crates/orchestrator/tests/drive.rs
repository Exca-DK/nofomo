mod common;

use common::{Harness, Receipt, Script, phase};
use tempo_agentic_domain::ExecStep;
use tempo_agentic_strategy::OrderState;

#[tokio::test]
async fn an_order_runs_to_filled_and_saves_before_every_side_effect() {
    let harness = Harness::new("filled", Script::default()).await;

    let order = harness.drive("o-1").await;

    assert!(matches!(order.state, OrderState::Filled { .. }));
    assert_eq!(phase(&harness.stored("o-1").await.state), "filled");
    // Each entry is the phase the row already held when the fake was called, so
    // this is the proof that nothing reached the network before being saved.
    assert_eq!(
        harness.seen(),
        vec!["swap_ready", "broadcasting", "submitted"]
    );
    harness.cleanup();
}

// The venue prepends an allowance, and once it confirms the venue stops asking
// for it, so the sequence repairs itself without anyone recording it.
#[tokio::test]
async fn an_allowance_runs_first_and_the_fill_records_the_swap_hash() {
    let harness = Harness::new(
        "approval",
        Script {
            steps: vec![
                vec![ExecStep::Approval, ExecStep::Swap],
                vec![ExecStep::Swap],
            ],
            ..Script::default()
        },
    )
    .await;

    let order = harness.drive("o-1").await;

    assert_eq!(harness.broadcasts(), 2);
    assert_eq!(
        order.state,
        OrderState::Filled {
            // The second signature, not the approval's.
            tx_hash: "0xhash1".into()
        }
    );
    assert_eq!(
        harness.seen(),
        vec![
            "swap_ready",
            "broadcasting",
            "submitted",
            "swap_ready",
            "broadcasting",
            "submitted"
        ]
    );
    harness.cleanup();
}

#[tokio::test]
async fn an_unmined_transaction_leaves_the_order_submitted() {
    let harness = Harness::new(
        "pending",
        Script {
            receipts: vec![Receipt::Pending],
            ..Script::default()
        },
    )
    .await;

    harness.drive("o-1").await;

    assert_eq!(phase(&harness.stored("o-1").await.state), "submitted");
    harness.cleanup();
}

// A node that will not answer says nothing about the trade. Failing the order
// here would abandon a transaction that is very likely on chain, so the pass
// must leave it alone and pick it up once the node recovers.
#[tokio::test]
async fn a_receipt_the_node_could_not_answer_is_retried_not_failed() {
    let harness = Harness::new(
        "rpc-error",
        Script {
            receipts: vec![Receipt::Error, Receipt::Success],
            ..Script::default()
        },
    )
    .await;

    let stalled = harness.drive("o-1").await;
    assert_eq!(phase(&stalled.state), "submitted");
    assert_eq!(phase(&harness.stored("o-1").await.state), "submitted");

    // The node answers on the next pass, and the same order finishes.
    let recovered = harness.drive("o-1").await;
    assert_eq!(
        recovered.state,
        OrderState::Filled {
            tx_hash: "0xhash0".into()
        }
    );
    harness.cleanup();
}

#[tokio::test]
async fn a_reverted_transaction_fails_the_order_and_keeps_its_hash() {
    let harness = Harness::new(
        "reverted",
        Script {
            receipts: vec![Receipt::Reverted],
            ..Script::default()
        },
    )
    .await;

    let order = harness.drive("o-1").await;

    assert_eq!(
        order.state,
        OrderState::Failed {
            tx_hash: Some("0xhash0".into()),
            reason: "reverted on-chain".into(),
        }
    );
    harness.cleanup();
}

// A stale quote lands here: Uniswap rejects it at build time, so the order fails
// before anything is broadcast and the level is free to fire again.
#[tokio::test]
async fn a_build_failure_stops_before_anything_is_broadcast() {
    let harness = Harness::new(
        "build-error",
        Script {
            build_fails: true,
            ..Script::default()
        },
    )
    .await;

    let order = harness.drive("o-1").await;

    assert_eq!(harness.broadcasts(), 0);
    let OrderState::Failed { tx_hash, reason } = &order.state else {
        panic!("expected a failed order, got {:?}", order.state);
    };
    assert!(tx_hash.is_none(), "nothing was signed, so there is no hash");
    assert!(reason.contains("quote expired"), "lost the cause: {reason}");
    harness.cleanup();
}

#[tokio::test]
async fn a_broadcast_failure_keeps_the_hash_it_tried_to_send() {
    let harness = Harness::new(
        "broadcast-error",
        Script {
            broadcast_fails: true,
            ..Script::default()
        },
    )
    .await;

    let order = harness.drive("o-1").await;

    let OrderState::Failed { tx_hash, .. } = &order.state else {
        panic!("expected a failed order, got {:?}", order.state);
    };
    assert_eq!(tx_hash.as_deref(), Some("0xhash0"));
    harness.cleanup();
}

// A venue stuck asking for the same approval would otherwise broadcast forever.
#[tokio::test]
async fn a_venue_that_never_advances_is_cut_off() {
    let harness = Harness::new(
        "spin",
        Script {
            steps: vec![vec![ExecStep::Approval, ExecStep::Swap]],
            ..Script::default()
        },
    )
    .await;

    let order = harness.drive("o-1").await;

    assert!(!order.is_terminal());
    // Twelve transitions, three per approval cycle.
    assert_eq!(harness.broadcasts(), 4);
    harness.cleanup();
}
