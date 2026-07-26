use tempo_agentic_chain::is_duplicate_submission;

// A re-broadcast after a crash must not look like a failure: the bytes are
// identical, so the node is reporting a transaction that is already accounted
// for, not a new problem.
#[test]
fn known_duplicate_phrasings_count_as_submitted() {
    for message in ["already known", "transaction already imported"] {
        assert!(is_duplicate_submission(message), "missed: {message}");
    }
}

// Ambiguous, and included anyway: it means either our transaction was mined, or
// somebody else took the nonce. The first resolves when the receipt turns up;
// the second produces none and is caught by the receipt deadline.
#[test]
fn a_spent_nonce_counts_as_submitted() {
    assert!(is_duplicate_submission("nonce too low"));
}

// The one that used to be swallowed. A *different* transaction holds the nonce,
// so ours will never land — calling it success parked the order on a hash that
// was in no mempool, and nothing could ever move it again.
#[test]
fn a_nonce_held_by_another_transaction_is_a_failure() {
    assert!(!is_duplicate_submission(
        "replacement transaction underpriced"
    ));
}

#[test]
fn duplicate_detection_ignores_case_and_surrounding_text() {
    assert!(is_duplicate_submission(
        "server returned an error response: error code -32000: ALREADY KNOWN"
    ));
}

#[test]
fn real_failures_are_not_swallowed() {
    for message in [
        "insufficient funds for gas * price + value",
        "intrinsic gas too low",
        "invalid sender",
        "connection refused",
    ] {
        assert!(!is_duplicate_submission(message), "swallowed: {message}");
    }
}
