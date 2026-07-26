mod common;

use common::{Harness, Receipt, Script, phase};
use tempo_agentic_domain::ExecStep;
use tempo_agentic_orchestrator::{RECEIPT_DEADLINE_SECS, SWAP_RETRY_CAP};
use tempo_agentic_strategy::{OrderState, OrderStatus};

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

// A refused send says nothing about the transaction, so the order keeps the
// signed bytes and waits. The next pass resends exactly those.
#[tokio::test]
async fn a_failed_broadcast_waits_instead_of_failing_the_order() {
    let harness = Harness::new(
        "broadcast-error",
        Script {
            broadcast_fails: true,
            ..Script::default()
        },
    )
    .await;

    let order = harness.drive("o-1").await;

    assert_eq!(phase(&order.state), "broadcasting");
    let stored = harness.stored("o-1").await;
    assert_eq!(stored.swap_attempts, 1);
    assert!(
        stored.swap_retry_after_ts.is_some(),
        "the schedule has to be on disk, or a restart would resend at once"
    );
    assert_eq!(stored.tx_hash(), Some("0xhash0"));
    harness.cleanup();
}

#[tokio::test]
async fn a_scheduled_retry_does_not_run_early() {
    let harness = Harness::new("backoff", Script::default()).await;
    let mut waiting = harness.stored("o-1").await;
    waiting.swap_retry_after_ts = Some(i64::MAX);
    harness.put(&waiting).await;

    harness.drive("o-1").await;

    assert_eq!(harness.signatures(), 0);
    assert_eq!(harness.broadcasts(), 0);
    assert_eq!(phase(&harness.stored("o-1").await.state), "swap_ready");
    harness.cleanup();
}

// Retries are not endless: a node that never accepts the bytes parks the order,
// which keeps the level blocked instead of letting it spend more gas.
#[tokio::test]
async fn broadcasts_that_keep_failing_end_in_quarantine() {
    let harness = Harness::new(
        "quarantine",
        Script {
            broadcast_fails: true,
            ..Script::default()
        },
    )
    .await;

    // Each pass burns one attempt and schedules the next; clearing the timer
    // stands in for waiting the backoff out.
    for _ in 0..=SWAP_RETRY_CAP {
        let mut ready = harness.stored("o-1").await;
        ready.swap_retry_after_ts = None;
        harness.put(&ready).await;
        harness.drive("o-1").await;
    }

    let order = harness.stored("o-1").await;
    let OrderState::SwapQuarantined { tx_hash, .. } = &order.state else {
        panic!("expected a quarantined order, got {:?}", order.state);
    };
    assert_eq!(
        tx_hash.as_deref(),
        Some("0xhash0"),
        "an operator needs the hash to check whether the bytes landed"
    );
    assert_eq!(order.status(), OrderStatus::Quarantined);
    assert!(order.is_terminal());
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

// The gate stops the one step that spends money and nothing before it: the order
// really is built and signed, so a blocked run exercises the whole path.
#[tokio::test]
async fn a_blocked_gate_signs_but_sends_nothing() {
    let harness = Harness::new(
        "gated",
        Script {
            allow_broadcast: false,
            ..Script::default()
        },
    )
    .await;

    let order = harness.drive("o-1").await;

    assert_eq!(harness.signatures(), 1, "the transaction has to be built");
    assert_eq!(harness.broadcasts(), 0, "and must not leave the process");
    let OrderState::Failed { tx_hash, reason } = &order.state else {
        panic!("expected a failed order, got {:?}", order.state);
    };
    assert!(tx_hash.is_none());
    assert!(
        reason.contains("MAINNET_SWAP"),
        "say how to open it: {reason}"
    );
    // Failed keeps the level free, so the dry run repeats instead of dying.
    assert_eq!(order.status(), OrderStatus::Failed);
    harness.cleanup();
}

// A nonce taken by somebody else produces a hash that is in no mempool, so no
// receipt is ever coming. Before the deadline existed this order — and its
// level — stayed stuck for good.
#[tokio::test]
async fn a_transaction_that_never_lands_gives_up_and_frees_the_level() {
    let harness = Harness::new(
        "receipt-deadline",
        Script {
            receipts: vec![Receipt::Pending],
            ..Script::default()
        },
    )
    .await;

    // First pass sends it and finds no receipt yet, which is normal.
    harness.drive("o-1").await;
    assert_eq!(phase(&harness.stored("o-1").await.state), "submitted");

    // Wind the clock back past the deadline rather than wait half an hour.
    let mut stale = harness.stored("o-1").await;
    let OrderState::Submitted {
        step,
        amount_in,
        tx_hash,
        withdraw_action_id,
        submitted_at,
    } = stale.state.clone()
    else {
        panic!("expected a submitted order, got {:?}", stale.state);
    };
    stale.state = OrderState::Submitted {
        step,
        amount_in,
        tx_hash,
        withdraw_action_id,
        submitted_at: submitted_at - RECEIPT_DEADLINE_SECS - 1,
    };
    harness.put(&stale).await;

    let given_up = harness.drive("o-1").await;

    let OrderState::Failed { tx_hash, reason } = &given_up.state else {
        panic!("expected a failed order, got {:?}", given_up.state);
    };
    assert_eq!(
        tx_hash.as_deref(),
        Some("0xhash0"),
        "the hash is the only way to find out later whether it landed"
    );
    assert!(
        reason.contains("may still land"),
        "lost the warning: {reason}"
    );
    // Failed is what lets the level try again, which is the point of giving up.
    assert_eq!(given_up.status(), OrderStatus::Failed);
    harness.cleanup();
}
