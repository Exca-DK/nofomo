use std::path::Path;
use std::str::FromStr;

use alloy::consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy::eips::eip2718::Encodable2718;
use alloy::primitives::{Address, Bytes, TxKind, U256};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use tempo_agentic_domain::{ChainFamily, EvmTx, SignedEvmTx};

use crate::error::VaultError;
use crate::secret_file::read_secret;

/// In-memory EVM signer.
pub struct EvmKeystore {
    signer: PrivateKeySigner,
    address: String,
}

impl EvmKeystore {
    /// Loads a hex key from an owner-only file.
    pub fn from_file(path: &Path) -> Result<Self, VaultError> {
        Self::from_hex(&read_secret(path)?)
    }

    /// Parses a hex private key.
    pub fn from_hex(hex: &str) -> Result<Self, VaultError> {
        let signer =
            PrivateKeySigner::from_str(hex.trim()).map_err(|error| VaultError::KeyLoad {
                family: ChainFamily::Evm,
                reason: error.to_string(),
            })?;
        Ok(Self::from_signer(signer))
    }

    pub fn generate() -> Self {
        Self::from_signer(PrivateKeySigner::random())
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// Returns the persisted key format.
    pub fn secret_material(&self) -> String {
        format!("0x{:x}", self.signer.to_bytes())
    }

    /// Signs an EIP-1559 transaction and derives its hash.
    pub fn sign(&self, tx: &EvmTx) -> Result<SignedEvmTx, VaultError> {
        let to = Address::from_str(&tx.to)
            .map_err(|_| VaultError::Sign(format!("invalid to address {}", tx.to)))?;
        let value = U256::from_str_radix(&tx.value, 10).map_err(|_| {
            VaultError::Sign(format!("value is not a decimal integer: {}", tx.value))
        })?;
        let input = Bytes::from_str(&tx.data)
            .map_err(|_| VaultError::Sign(format!("invalid calldata: {}", tx.data)))?;

        let unsigned = TxEip1559 {
            chain_id: tx.chain_id,
            nonce: tx.nonce,
            gas_limit: tx.gas_limit,
            max_fee_per_gas: tx.max_fee_per_gas,
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas,
            to: TxKind::Call(to),
            value,
            access_list: Default::default(),
            input,
        };

        let signature = self
            .signer
            .sign_hash_sync(&unsigned.signature_hash())
            .map_err(|error| VaultError::Sign(error.to_string()))?;
        let envelope = TxEnvelope::Eip1559(unsigned.into_signed(signature));

        Ok(SignedEvmTx {
            raw: format!("0x{}", hex_encode(&envelope.encoded_2718())),
            hash: format!("{:#x}", envelope.tx_hash()),
        })
    }

    fn from_signer(signer: PrivateKeySigner) -> Self {
        let address = signer.address().to_string();
        Self { signer, address }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const DEV_ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

    #[test]
    fn loads_the_address_from_a_hex_key() {
        let keystore = EvmKeystore::from_hex(DEV_KEY).expect("load");
        assert_eq!(keystore.address(), DEV_ADDRESS);
    }

    #[test]
    fn rejects_a_key_that_is_not_secp256k1() {
        assert!(matches!(
            EvmKeystore::from_hex("not-a-key"),
            Err(VaultError::KeyLoad { .. })
        ));
    }

    #[test]
    fn a_generated_key_round_trips_through_its_secret_material() {
        let generated = EvmKeystore::generate();
        let reloaded = EvmKeystore::from_hex(&generated.secret_material()).expect("reload");
        assert_eq!(reloaded.address(), generated.address());
    }
}
