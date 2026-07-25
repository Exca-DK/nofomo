use alloy::consensus::{Transaction, TxEnvelope};
use alloy::eips::eip2718::Decodable2718;
use alloy::primitives::{Bytes, U256};
use tempo_agentic_domain::{Signer, UnsignedTx};
use tempo_agentic_vault::EvmSigner;

// Anvil's first deterministic account, so the expected address is fixed.
const TEST_KEY: [u8; 32] = [
    0xac, 0x09, 0x74, 0xbe, 0xc3, 0x9a, 0x17, 0xe3, 0x6b, 0xa4, 0xa6, 0xb4, 0xd2, 0x38, 0xff, 0x94,
    0x4b, 0xac, 0xb4, 0x78, 0xcb, 0xed, 0x5e, 0xfc, 0xae, 0x78, 0x4d, 0x7b, 0xf4, 0xf2, 0xff, 0x80,
];
const TEST_ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

fn unsigned() -> UnsignedTx {
    UnsignedTx {
        chain_id: 8453,
        nonce: 7,
        gas_limit: 210_000,
        max_fee_per_gas: 1_500_000_000,
        max_priority_fee_per_gas: 100_000_000,
        to: "0x0000000085E102724e78eCd2F45DC9cA239Affad".to_string(),
        value: "0".to_string(),
        data: "0x095ea7b3".to_string(),
    }
}

#[tokio::test]
async fn signing_preserves_every_field_and_derives_the_hash_offline() {
    let signer = EvmSigner::from_key(&TEST_KEY).unwrap();
    assert_eq!(signer.address(), TEST_ADDRESS);

    let tx = unsigned();
    let signed = signer.sign(&tx).await.unwrap();

    let bytes = Bytes::from_str_radix_hex(&signed.raw);
    let envelope = TxEnvelope::decode_2718(&mut bytes.as_ref()).unwrap();

    assert_eq!(envelope.chain_id(), Some(tx.chain_id));
    assert_eq!(envelope.nonce(), tx.nonce);
    assert_eq!(envelope.gas_limit(), tx.gas_limit);
    assert_eq!(envelope.max_fee_per_gas(), tx.max_fee_per_gas);
    assert_eq!(
        envelope.max_priority_fee_per_gas(),
        Some(tx.max_priority_fee_per_gas)
    );
    assert_eq!(envelope.to().unwrap().to_string(), tx.to);
    assert_eq!(envelope.value(), U256::ZERO);
    assert_eq!(envelope.input().to_string(), tx.data);

    // The hash the signer reported must be the one the decoded bytes carry:
    // that is what lets it be persisted before the transaction is broadcast.
    assert_eq!(format!("{:#x}", envelope.tx_hash()), signed.hash);
}

#[tokio::test]
async fn the_same_transaction_always_signs_to_the_same_bytes() {
    let signer = EvmSigner::from_key(&TEST_KEY).unwrap();
    let first = signer.sign(&unsigned()).await.unwrap();
    let second = signer.sign(&unsigned()).await.unwrap();
    assert_eq!(first, second);
}

#[tokio::test]
async fn a_different_nonce_produces_a_different_transaction() {
    let signer = EvmSigner::from_key(&TEST_KEY).unwrap();
    let first = signer.sign(&unsigned()).await.unwrap();
    let mut other = unsigned();
    other.nonce += 1;
    let second = signer.sign(&other).await.unwrap();
    assert_ne!(first.hash, second.hash);
}

#[tokio::test]
async fn malformed_fields_are_rejected_rather_than_signed() {
    let signer = EvmSigner::from_key(&TEST_KEY).unwrap();

    let mut bad_to = unsigned();
    bad_to.to = "not-an-address".to_string();
    assert!(signer.sign(&bad_to).await.is_err());

    let mut bad_value = unsigned();
    bad_value.value = "0x2a".to_string();
    assert!(signer.sign(&bad_value).await.is_err());

    let mut bad_data = unsigned();
    bad_data.data = "zz".to_string();
    assert!(signer.sign(&bad_data).await.is_err());
}

#[test]
fn rejects_a_key_that_is_not_a_valid_scalar() {
    assert!(EvmSigner::from_key(&[0u8; 32]).is_err());
}

trait HexBytes {
    fn from_str_radix_hex(value: &str) -> Bytes;
}

impl HexBytes for Bytes {
    fn from_str_radix_hex(value: &str) -> Bytes {
        let trimmed = value.strip_prefix("0x").unwrap_or(value);
        let bytes: Vec<u8> = (0..trimmed.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&trimmed[i..i + 2], 16).expect("valid hex"))
            .collect();
        Bytes::from(bytes)
    }
}
