use std::path::Path;
use std::str::FromStr;

use alloy::consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy::eips::eip2718::Encodable2718;
use alloy::primitives::{Address, Bytes, TxKind, U256};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use async_trait::async_trait;
use tempo_agentic_domain::{SignedTx, Signer, UnsignedTx};

/// EVM signer with a key decrypted once at startup.
pub struct EvmSigner {
    signer: PrivateKeySigner,
    address: String,
}

impl EvmSigner {
    /// Decrypts a Foundry keystore using `password_file`.
    pub fn from_keystore(keystore_path: &Path, password_file: &Path) -> Result<Self> {
        let password = std::fs::read_to_string(password_file)
            .with_context(|| format!("cannot read password file {}", password_file.display()))?;
        let key = eth_keystore::decrypt_key(keystore_path, password.trim())
            .with_context(|| format!("cannot decrypt keystore {}", keystore_path.display()))?;
        Self::from_key(&key)
    }

    /// Returns an error if the bytes are not a valid secp256k1 private key.
    pub fn from_key(key: &[u8]) -> Result<Self> {
        let signer =
            PrivateKeySigner::from_slice(key).context("invalid private key in keystore")?;
        let address = signer.address().to_string();
        Ok(Self { signer, address })
    }
}

#[async_trait]
impl Signer for EvmSigner {
    fn address(&self) -> &str {
        &self.address
    }

    async fn sign(&self, tx: &UnsignedTx) -> Result<SignedTx> {
        let to =
            Address::from_str(&tx.to).with_context(|| format!("invalid to address {}", tx.to))?;
        let value = U256::from_str_radix(&tx.value, 10)
            .with_context(|| format!("value is not a decimal integer: {}", tx.value))?;
        let input =
            Bytes::from_str(&tx.data).with_context(|| format!("invalid calldata: {}", tx.data))?;

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
            .context("signing failed")?;
        let envelope = TxEnvelope::Eip1559(unsigned.into_signed(signature));

        // Signed bytes determine both values before broadcast.
        Ok(SignedTx {
            raw: format!("0x{}", hex_encode(&envelope.encoded_2718())),
            hash: format!("{:#x}", envelope.tx_hash()),
        })
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
