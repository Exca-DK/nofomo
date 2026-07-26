use alloy::consensus::{Transaction, TxEnvelope};
use alloy::eips::eip2718::Decodable2718;
use alloy::primitives::{Bytes, U256};
use tempo_agentic_domain::{ChainFamily, EvmTx, SignedTx, Signer, UnsignedTx};
use tempo_agentic_vault::{Vault, VaultSigner};

const TEST_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const TEST_ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

fn vault() -> Vault {
    let mut vault = Vault::new();
    vault.add(VaultSigner::import(ChainFamily::Evm, TEST_KEY).expect("import the test key"));
    vault
}

fn evm_tx() -> EvmTx {
    EvmTx {
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

async fn sign(vault: &Vault, tx: EvmTx) -> anyhow::Result<tempo_agentic_domain::SignedEvmTx> {
    match vault.sign(&UnsignedTx::Evm(tx)).await? {
        SignedTx::Evm(signed) => Ok(signed),
        SignedTx::Sui(_) => panic!("an EVM transaction must come back signed for EVM"),
    }
}

#[tokio::test]
async fn signing_preserves_every_field_and_derives_the_hash_offline() {
    let vault = vault();
    assert_eq!(vault.address(ChainFamily::Evm).unwrap(), TEST_ADDRESS);

    let tx = evm_tx();
    let signed = sign(&vault, tx.clone()).await.unwrap();

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

    assert_eq!(format!("{:#x}", envelope.tx_hash()), signed.hash);
}

#[tokio::test]
async fn the_same_transaction_always_signs_to_the_same_bytes() {
    let vault = vault();
    let first = sign(&vault, evm_tx()).await.unwrap();
    let second = sign(&vault, evm_tx()).await.unwrap();
    assert_eq!(first, second);
}

#[tokio::test]
async fn a_different_nonce_produces_a_different_transaction() {
    let vault = vault();
    let first = sign(&vault, evm_tx()).await.unwrap();
    let mut other = evm_tx();
    other.nonce += 1;
    let second = sign(&vault, other).await.unwrap();
    assert_ne!(first.hash, second.hash);
}

#[tokio::test]
async fn malformed_fields_are_rejected_rather_than_signed() {
    let vault = vault();

    let mut bad_to = evm_tx();
    bad_to.to = "not-an-address".to_string();
    assert!(sign(&vault, bad_to).await.is_err());

    let mut bad_value = evm_tx();
    bad_value.value = "0x2a".to_string();
    assert!(sign(&vault, bad_value).await.is_err());

    let mut bad_data = evm_tx();
    bad_data.data = "zz".to_string();
    assert!(sign(&vault, bad_data).await.is_err());
}

#[test]
fn rejects_a_key_that_is_not_a_valid_scalar() {
    let zero = format!("0x{}", "00".repeat(32));
    assert!(VaultSigner::import(ChainFamily::Evm, &zero).is_err());
}

#[test]
fn a_key_imported_for_one_family_is_rejected_by_the_other() {
    assert!(VaultSigner::import(ChainFamily::Sui, TEST_KEY).is_err());
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
