use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sui_sdk_types::{Transaction, UserSignature};

/// Chain state needed to finalize a transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxContext {
    Evm {
        chain_id: u64,
        nonce: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
    },
    Sui {
        gas_price: u64,
        gas_budget: u64,
    },
}

/// An EIP-1559 transaction ready to sign, using chain-independent types.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct EvmTx {
    pub chain_id: u64,
    pub nonce: u64,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    /// Recipient, 0x-prefixed.
    pub to: String,
    /// Native amount in decimal wei, matching how amounts travel elsewhere.
    pub value: String,
    /// Calldata, 0x-prefixed.
    pub data: String,
}

/// Signed bytes and their locally derived hash, persisted before broadcast.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct SignedEvmTx {
    /// Raw EIP-2718 bytes, 0x-prefixed, ready for `eth_sendRawTransaction`.
    pub raw: String,
    pub hash: String,
}

/// Sui transaction with its detached signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedSuiTx {
    pub transaction: Transaction,
    pub signature: UserSignature,
}

impl SignedSuiTx {
    /// Digest known before broadcast.
    pub fn digest(&self) -> String {
        self.transaction.digest().to_string()
    }

    /// Encodes the transaction and signature for persistence.
    pub fn to_wire(&self) -> Result<String> {
        let wire = SuiWire {
            transaction: bcs_bytes(&self.transaction)?,
            signature: self.signature.to_bytes(),
        };
        serde_json::to_string(&wire).context("cannot encode the signed Sui transaction")
    }

    /// Decodes a value produced by [`SignedSuiTx::to_wire`].
    pub fn from_wire(raw: &str) -> Result<Self> {
        let wire: SuiWire =
            serde_json::from_str(raw).context("stored Sui transaction is not valid JSON")?;
        Ok(Self {
            transaction: bcs::from_bytes(&wire.transaction)
                .context("stored Sui transaction is not valid BCS")?,
            signature: UserSignature::from_bytes(&wire.signature)
                .map_err(|error| anyhow::anyhow!("stored Sui signature is unusable: {error}"))?,
        })
    }
}

#[derive(Deserialize, Serialize)]
struct SuiWire {
    transaction: Vec<u8>,
    signature: Vec<u8>,
}

fn bcs_bytes(transaction: &Transaction) -> Result<Vec<u8>> {
    bcs::to_bytes(transaction).context("cannot encode the Sui transaction")
}

/// Unsigned transaction by chain family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnsignedTx {
    Evm(EvmTx),
    Sui(Box<Transaction>),
}

/// Signed transaction by chain family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignedTx {
    Evm(SignedEvmTx),
    Sui(Box<SignedSuiTx>),
}

impl SignedTx {
    /// Hash known before broadcast.
    pub fn hash(&self) -> String {
        match self {
            Self::Evm(tx) => tx.hash.clone(),
            Self::Sui(tx) => tx.digest(),
        }
    }

    /// Encodes the transaction for persistence.
    pub fn to_wire(&self) -> Result<String> {
        match self {
            Self::Evm(tx) => Ok(tx.raw.clone()),
            Self::Sui(tx) => tx.to_wire(),
        }
    }
}

/// Outcome of looking up a transaction receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptStatus {
    /// Not mined yet; check again later.
    Pending,
    Success,
    Reverted,
}
