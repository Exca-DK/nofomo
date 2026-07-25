use tempo_agentic_domain::{SignedTx, UnsignedTx};

fn unsigned_tx() -> UnsignedTx {
    UnsignedTx {
        chain_id: 8453,
        nonce: 7,
        gas_limit: 210_000,
        max_fee_per_gas: 50_000_000_000,
        max_priority_fee_per_gas: 1_500_000_000,
        to: "0x0000000085E102724e78eCd2F45DC9cA239Affad".into(),
        value: "0".into(),
        data: "0x095ea7b3".into(),
    }
}

fn signed_tx() -> SignedTx {
    SignedTx {
        raw: "0x02f8b0...".into(),
        hash: "0xdeadbeef00000000000000000000000000000000000000000000000000000000".into(),
    }
}

#[test]
fn unsigned_tx_round_trips_through_json() {
    let original = unsigned_tx();
    let json = serde_json::to_string(&original).unwrap();
    let decoded: UnsignedTx = serde_json::from_str(&json).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn signed_tx_round_trips_through_json() {
    let original = signed_tx();
    let json = serde_json::to_string(&original).unwrap();
    let decoded: SignedTx = serde_json::from_str(&json).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn signed_tx_survives_a_value_round_trip_too() {
    // `crates/storage` stores state as a `serde_json::Value` column, not a
    // string, so the `Value` round trip matters as much as the string one.
    let original = signed_tx();
    let value = serde_json::to_value(&original).unwrap();
    let decoded: SignedTx = serde_json::from_value(value).unwrap();
    assert_eq!(original, decoded);
}
