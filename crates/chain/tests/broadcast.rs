use tempo_agentic_chain::is_duplicate_submission;

// A re-broadcast after a crash must not look like a failure: the bytes are
// identical, so the node is reporting a transaction that is already accounted
// for, not a new problem.
#[test]
fn known_duplicate_phrasings_count_as_submitted() {
    for message in [
        "already known",
        "transaction already imported",
        "nonce too low",
        "replacement transaction underpriced",
    ] {
        assert!(is_duplicate_submission(message), "missed: {message}");
    }
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
