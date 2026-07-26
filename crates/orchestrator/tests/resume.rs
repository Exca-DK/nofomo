mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::U256;
use common::{BASE_ID, Harness, Script, order};
use tempo_agentic_domain::ExecStep;
use tempo_agentic_strategy::OrderState;

const AMOUNT: u64 = 1_000_000;
/// Signed bytes that a resumed order must reuse.
const SIGNED: &str = "0x02f8730182deadbeef";
const HASH: &str = "0xfeedfacefeedface";

#[tokio::test]
async fn a_broadcasting_order_resumes_with_the_bytes_it_already_signed() {
    let harness = Harness::new("resume-broadcasting", Script::default()).await;
    let mut interrupted = order("o-1", BASE_ID);
    interrupted.state = OrderState::Broadcasting {
        step: ExecStep::Swap,
        amount_in: U256::from(AMOUNT),
        signed_tx: SIGNED.into(),
        tx_hash: HASH.into(),
        withdraw_action_id: None,
    };
    harness.put(&interrupted).await;

    // Simulate a crash; only disk state remains.
    let harness = harness.reopen(Script::default()).await;
    let resumed = harness.drive("o-1").await;

    assert_eq!(harness.sent(), vec![SIGNED.to_string()]);
    assert_eq!(harness.signatures(), 0, "resuming must not sign again");
    assert_eq!(
        harness.steps_calls(),
        0,
        "resuming must not rebuild the transaction"
    );
    assert_eq!(
        resumed.state,
        OrderState::Filled {
            tx_hash: HASH.into()
        }
    );
    harness.cleanup();
}

// Resume receipt polling without rebroadcasting.
#[tokio::test]
async fn a_submitted_order_resumes_into_a_receipt_check() {
    let harness = Harness::new("resume-submitted", Script::default()).await;
    let mut interrupted = order("o-1", BASE_ID);
    interrupted.state = OrderState::Submitted {
        step: ExecStep::Swap,
        amount_in: U256::from(AMOUNT),
        tx_hash: HASH.into(),
        withdraw_action_id: None,
        submitted_at: now_unix(),
    };
    harness.put(&interrupted).await;

    let harness = harness.reopen(Script::default()).await;
    let resumed = harness.drive("o-1").await;

    assert_eq!(harness.broadcasts(), 0);
    assert_eq!(harness.signatures(), 0);
    assert_eq!(
        resumed.state,
        OrderState::Filled {
            tx_hash: HASH.into()
        }
    );
    harness.cleanup();
}

// Re-derive steps so a confirmed approval is not repeated.
#[tokio::test]
async fn a_resumed_order_takes_the_step_the_venue_still_wants() {
    let harness = Harness::new("resume-allowance", Script::default()).await;
    let mut interrupted = order("o-1", BASE_ID);
    interrupted.state = OrderState::SwapReady {
        step: ExecStep::Approval,
        amount_in: U256::from(AMOUNT),
        withdraw_action_id: None,
    };
    harness.put(&interrupted).await;

    // The reopened venue reports the allowance as already granted.
    let harness = harness
        .reopen(Script {
            steps: vec![vec![ExecStep::Swap]],
            ..Script::default()
        })
        .await;
    let resumed = harness.drive("o-1").await;

    assert_eq!(harness.broadcasts(), 1, "the allowance must not be re-sent");
    assert!(matches!(resumed.state, OrderState::Filled { .. }));
    harness.cleanup();
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
