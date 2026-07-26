use tempo_agentic_chain::is_duplicate_submission;

// Identical rebroadcasts after a crash count as accepted.
#[test]
fn known_duplicate_phrasings_count_as_submitted() {
    for message in ["already known", "transaction already imported"] {
        assert!(is_duplicate_submission(message), "missed: {message}");
    }
}

// `nonce too low` is resolved later by receipt or deadline.
#[test]
fn a_spent_nonce_counts_as_submitted() {
    assert!(is_duplicate_submission("nonce too low"));
}

// Replacement-underpriced means different bytes own the nonce.
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
