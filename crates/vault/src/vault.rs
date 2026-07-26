use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use tempo_agentic_domain::{ChainFamily, SignedTx, Signer, UnsignedTx};

use crate::error::VaultError;
use crate::evm::EvmKeystore;
use crate::sui::SuiKeystore;

/// Signer for one chain family.
pub enum VaultSigner {
    Evm(EvmKeystore),
    Sui(SuiKeystore),
}

impl VaultSigner {
    pub fn generate(family: ChainFamily) -> Self {
        match family {
            ChainFamily::Evm => Self::Evm(EvmKeystore::generate()),
            ChainFamily::Sui => Self::Sui(SuiKeystore::generate()),
        }
    }

    /// Parses key material without writing it.
    pub fn import(family: ChainFamily, key: &str) -> Result<Self, VaultError> {
        match family {
            ChainFamily::Evm => EvmKeystore::from_hex(key).map(Self::Evm),
            ChainFamily::Sui => SuiKeystore::from_base64(key).map(Self::Sui),
        }
    }

    /// Loads a signer from an owner-only file.
    pub fn load(family: ChainFamily, path: &Path) -> Result<Self, VaultError> {
        match family {
            ChainFamily::Evm => EvmKeystore::from_file(path).map(Self::Evm),
            ChainFamily::Sui => SuiKeystore::from_file(path).map(Self::Sui),
        }
    }

    pub fn family(&self) -> ChainFamily {
        match self {
            Self::Evm(_) => ChainFamily::Evm,
            Self::Sui(_) => ChainFamily::Sui,
        }
    }

    pub fn address(&self) -> &str {
        match self {
            Self::Evm(keystore) => keystore.address(),
            Self::Sui(keystore) => keystore.address(),
        }
    }

    /// Returns the persisted key format.
    pub fn secret_material(&self) -> String {
        match self {
            Self::Evm(keystore) => keystore.secret_material(),
            Self::Sui(keystore) => keystore.secret_material(),
        }
    }

    fn sign(&self, tx: &UnsignedTx) -> Result<SignedTx, VaultError> {
        match (self, tx) {
            (Self::Evm(keystore), UnsignedTx::Evm(tx)) => keystore.sign(tx).map(SignedTx::Evm),
            (Self::Sui(keystore), UnsignedTx::Sui(tx)) => keystore
                .sign(tx)
                .map(|signed| SignedTx::Sui(Box::new(signed))),
            _ => Err(VaultError::WrongFamily {
                family: self.family(),
            }),
        }
    }
}

/// In-memory signer per chain family.
#[derive(Default)]
pub struct Vault {
    signers: BTreeMap<ChainFamily, VaultSigner>,
}

impl Vault {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces a signer.
    pub fn add(&mut self, signer: VaultSigner) {
        self.signers.insert(signer.family(), signer);
    }

    fn signer(&self, family: ChainFamily) -> Result<&VaultSigner, VaultError> {
        self.signers.get(&family).ok_or(VaultError::NoKey(family))
    }
}

#[async_trait]
impl Signer for Vault {
    fn address(&self, family: ChainFamily) -> Result<&str> {
        Ok(self.signer(family)?.address())
    }

    async fn sign(&self, tx: &UnsignedTx) -> Result<SignedTx> {
        let family = match tx {
            UnsignedTx::Evm(_) => ChainFamily::Evm,
            UnsignedTx::Sui(_) => ChainFamily::Sui,
        };
        Ok(self.signer(family)?.sign(tx)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempo_agentic_domain::EvmTx;

    const DEV_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    fn evm_vault() -> Vault {
        let mut vault = Vault::new();
        vault.add(VaultSigner::import(ChainFamily::Evm, DEV_KEY).expect("import"));
        vault
    }

    fn evm_tx() -> UnsignedTx {
        UnsignedTx::Evm(EvmTx {
            chain_id: 8453,
            nonce: 0,
            gas_limit: 21_000,
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
            to: "0x0000000000000000000000000000000000000001".into(),
            value: "0".into(),
            data: "0x".into(),
        })
    }

    #[test]
    fn reports_the_address_of_the_family_it_holds_a_key_for() {
        let vault = evm_vault();
        let keystore = VaultSigner::import(ChainFamily::Evm, DEV_KEY).expect("import");
        assert_eq!(
            vault.address(ChainFamily::Evm).expect("evm"),
            keystore.address()
        );
    }

    #[test]
    fn refuses_an_address_for_a_family_with_no_key() {
        assert!(evm_vault().address(ChainFamily::Sui).is_err());
    }

    #[tokio::test]
    async fn routes_a_transaction_to_the_key_of_its_own_family() {
        let signed = evm_vault().sign(&evm_tx()).await.expect("sign");
        assert!(matches!(signed, SignedTx::Evm(_)));
    }

    #[tokio::test]
    async fn refuses_to_sign_for_a_family_with_no_key() {
        let vault = evm_vault();
        let tx = UnsignedTx::Sui(Box::new(sui_transaction()));
        assert!(vault.sign(&tx).await.is_err());
    }

    fn sui_transaction() -> sui_sdk_types::Transaction {
        use sui_sdk_types::{
            Address, Digest, GasPayment, ObjectReference, ProgrammableTransaction,
            TransactionExpiration, TransactionKind,
        };

        let sender = Address::new([3; 32]);
        sui_sdk_types::Transaction {
            kind: TransactionKind::ProgrammableTransaction(ProgrammableTransaction {
                inputs: vec![],
                commands: vec![],
            }),
            sender,
            gas_payment: GasPayment {
                objects: vec![ObjectReference::new(
                    Address::new([1; 32]),
                    1,
                    Digest::new([2; 32]),
                )],
                owner: sender,
                price: 1_000,
                budget: 10_000_000,
            },
            expiration: TransactionExpiration::None,
        }
    }
}
