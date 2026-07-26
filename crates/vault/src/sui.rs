use std::path::Path;

use sui_crypto::SuiSigner;
use sui_crypto::ed25519::Ed25519PrivateKey;
use sui_crypto::simple::SimpleKeypair;
use sui_sdk_types::Transaction;
use tempo_agentic_domain::{ChainFamily, SignedSuiTx};

use crate::error::VaultError;
use crate::secret_file::read_secret;

/// In-memory Sui signer.
pub struct SuiKeystore {
    keypair: SimpleKeypair,
    address: String,
}

impl SuiKeystore {
    /// Loads a base64 keypair from an owner-only file.
    pub fn from_file(path: &Path) -> Result<Self, VaultError> {
        Self::from_base64(&read_secret(path)?)
    }

    /// Parses the base64 format exported by `sui keytool`.
    pub fn from_base64(key: &str) -> Result<Self, VaultError> {
        let keypair =
            SimpleKeypair::from_base64(key.trim()).map_err(|error| VaultError::KeyLoad {
                family: ChainFamily::Sui,
                reason: error.to_string(),
            })?;
        Ok(Self::from_keypair(keypair))
    }

    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        Self::from_keypair(SimpleKeypair::from(Ed25519PrivateKey::generate(&mut rng)))
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// Returns the persisted key format.
    pub fn secret_material(&self) -> String {
        self.keypair.to_base64()
    }

    /// Signs a transaction only when this account is its sender.
    pub fn sign(&self, tx: &Transaction) -> Result<SignedSuiTx, VaultError> {
        if tx.sender.to_string() != self.address {
            return Err(VaultError::Sign(format!(
                "transaction sender {} is not the vault account {}",
                tx.sender, self.address
            )));
        }
        let signature = self
            .keypair
            .sign_transaction(tx)
            .map_err(|error| VaultError::Sign(error.to_string()))?;
        Ok(SignedSuiTx {
            transaction: tx.clone(),
            signature,
        })
    }

    fn from_keypair(keypair: SimpleKeypair) -> Self {
        let address = keypair.verifying_key().derive_address().to_string();
        Self { keypair, address }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sui_sdk_types::{
        Address, Digest, GasPayment, ObjectReference, ProgrammableTransaction,
        TransactionExpiration, TransactionKind,
    };

    fn transaction_from(sender: Address) -> Transaction {
        Transaction {
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

    #[test]
    fn rejects_a_key_that_is_not_a_sui_keypair() {
        assert!(matches!(
            SuiKeystore::from_base64("not-a-key"),
            Err(VaultError::KeyLoad { .. })
        ));
    }

    #[test]
    fn a_generated_key_round_trips_through_its_secret_material() {
        let generated = SuiKeystore::generate();
        let reloaded = SuiKeystore::from_base64(&generated.secret_material()).expect("reload");
        assert_eq!(reloaded.address(), generated.address());
    }

    #[test]
    fn signs_a_transaction_it_is_the_sender_of() {
        let keystore = SuiKeystore::generate();
        let sender = keystore.address().parse().expect("own address parses");
        assert!(keystore.sign(&transaction_from(sender)).is_ok());
    }

    #[test]
    fn refuses_a_transaction_built_for_another_sender() {
        let keystore = SuiKeystore::generate();
        let stranger = SuiKeystore::generate();
        let sender = stranger.address().parse().expect("address parses");

        assert!(matches!(
            keystore.sign(&transaction_from(sender)),
            Err(VaultError::Sign(_))
        ));
    }

    #[test]
    fn a_signed_transaction_survives_the_form_an_order_persists_it_in() {
        let keystore = SuiKeystore::generate();
        let sender = keystore.address().parse().expect("own address parses");
        let signed = keystore.sign(&transaction_from(sender)).expect("sign");

        let restored = SignedSuiTx::from_wire(&signed.to_wire().expect("encode")).expect("decode");

        assert_eq!(restored, signed);
        assert_eq!(restored.digest(), signed.digest());
    }

    #[test]
    fn the_reported_digest_is_the_transactions_own() {
        let keystore = SuiKeystore::generate();
        let sender = keystore.address().parse().expect("own address parses");
        let transaction = transaction_from(sender);
        let signed = keystore.sign(&transaction).expect("sign");

        assert_eq!(signed.digest(), transaction.digest().to_string());
    }
}
