use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use sui_rpc::Client;
use sui_rpc::field::{FieldMask, FieldMaskUtil};
use sui_rpc::proto::sui::rpc::v2::{
    ExecuteTransactionRequest, GetEpochRequest, GetTransactionRequest, SimulateTransactionRequest,
};
use tempo_agentic_domain::{ChainClient, ChainId, DryRun, ReceiptStatus, SignedTx, TxContext};

/// Wallet-free Sui node client.
pub struct SuiChainClient {
    rpc_url: String,
    /// Per-transaction gas cap in MIST.
    gas_budget: u64,
}

impl SuiChainClient {
    /// Validates the RPC URL.
    pub fn new(rpc_url: &str, gas_budget: u64) -> Result<Self> {
        Client::new(rpc_url).context("invalid Sui RPC URL")?;
        Ok(Self {
            rpc_url: rpc_url.to_string(),
            gas_budget,
        })
    }

    fn client(&self) -> Result<Client> {
        Client::new(self.rpc_url.as_str()).context("failed to build Sui RPC client")
    }
}

#[async_trait]
impl ChainClient for SuiChainClient {
    fn chain(&self) -> ChainId {
        ChainId::Sui
    }

    async fn tx_context(&self, _from: &str) -> Result<TxContext> {
        let mut request = GetEpochRequest::default();
        request.read_mask = Some(FieldMask::from_paths(["reference_gas_price"]));
        let response = self
            .client()?
            .ledger_client()
            .get_epoch(request)
            .await
            .context("failed to read the current Sui epoch")?
            .into_inner();

        let gas_price = response
            .epoch
            .and_then(|epoch| epoch.reference_gas_price)
            .context("the Sui epoch carries no reference gas price")?;

        Ok(TxContext::Sui {
            gas_price,
            gas_budget: self.gas_budget,
        })
    }

    async fn broadcast(&self, signed: &SignedTx) -> Result<String> {
        let SignedTx::Sui(signed) = signed else {
            bail!("a Sui node cannot broadcast a transaction from another chain family");
        };

        let mut request = ExecuteTransactionRequest::default();
        request.transaction = Some(signed.transaction.clone().into());
        request.signatures = vec![signed.signature.clone().into()];
        request.read_mask = Some(FieldMask::from_paths(["digest"]));

        match self
            .client()?
            .execution_client()
            .execute_transaction(request)
            .await
        {
            Ok(response) => {
                let digest = response
                    .into_inner()
                    .transaction
                    .and_then(|executed| executed.digest)
                    .context("the Sui execution response carries no digest")?;
                Ok(digest)
            }
            Err(error) => {
                let message = error.to_string();
                // Duplicate execution confirms this signed digest.
                if is_already_executed(&message) {
                    tracing::info!(
                        tx_digest = %signed.digest(),
                        %message,
                        "node reports the transaction is already executed; treating as sent"
                    );
                    return Ok(signed.digest());
                }
                bail!("Sui transaction execution failed: {message}")
            }
        }
    }

    async fn confirmation(&self, tx_hash: &str) -> Result<ReceiptStatus> {
        let mut request = GetTransactionRequest::default();
        request.digest = Some(tx_hash.to_string());
        request.read_mask = Some(FieldMask::from_paths(["effects.status"]));

        let response = match self
            .client()?
            .ledger_client()
            .get_transaction(request)
            .await
        {
            Ok(response) => response.into_inner(),
            // Unknown means pending.
            Err(error) if error.code() == tonic::Code::NotFound => {
                return Ok(ReceiptStatus::Pending);
            }
            Err(error) => bail!("failed to read the Sui transaction: {error}"),
        };

        let status = response
            .transaction
            .and_then(|executed| executed.effects)
            .and_then(|effects| effects.status);
        match status.and_then(|status| status.success) {
            Some(true) => Ok(ReceiptStatus::Success),
            Some(false) => Ok(ReceiptStatus::Reverted),
            None => Ok(ReceiptStatus::Pending),
        }
    }

    async fn dry_run(&self, signed: &SignedTx) -> Result<DryRun> {
        let SignedTx::Sui(signed) = signed else {
            return Ok(DryRun::Unsupported);
        };

        // Simulation takes the transaction alone; the signature is not checked.
        let mut request = SimulateTransactionRequest::default();
        request.transaction = Some(signed.transaction.clone().into());
        request.read_mask = Some(FieldMask::from_paths(["transaction.effects.status"]));

        let response = self
            .client()?
            .execution_client()
            .simulate_transaction(request)
            .await
            .context("failed to simulate the Sui transaction")?
            .into_inner();

        let status = response
            .transaction
            .and_then(|executed| executed.effects)
            .and_then(|effects| effects.status);
        Ok(match status {
            Some(status) if status.success == Some(true) => DryRun::Succeeded,
            Some(status) => DryRun::Failed(
                status
                    .error
                    .and_then(|error| error.description)
                    .unwrap_or_else(|| "the node gave no reason".to_string()),
            ),
            None => DryRun::Failed("the node returned no execution status".to_string()),
        })
    }
}

// Duplicate execution is reported as an error.
fn is_already_executed(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    ["already executed", "already finalized", "alreadyexecuted"]
        .iter()
        .any(|phrase| message.contains(phrase))
}

#[cfg(test)]
mod tests {
    use super::is_already_executed;

    #[test]
    fn recognizes_a_transaction_the_network_already_has() {
        assert!(is_already_executed(
            "status: AlreadyExists, message: transaction already executed"
        ));
        assert!(!is_already_executed("insufficient gas"));
    }
}
