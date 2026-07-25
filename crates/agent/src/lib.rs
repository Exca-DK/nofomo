use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use tempo_agentic_chain::EvmChainClient;
use tempo_agentic_config::{Config, EvmChain};

use tempo_agentic_cetus::CetusVenue;
use tempo_agentic_domain::{
    AuditStore, ChainClient, ExecuteTradeRequest, ExecutionPlan, ExecutionView, MarketResearch,
    MarketResearchRequest, QuoteTradeRequest, QuoteView, ReceiptStatus, Signer, StoredQuote,
    TradeVenue, TransactionReference, unix_now, unix_now_nanos,
};
use tempo_agentic_graph::GraphClient;
use tempo_agentic_uniswap::UniswapVenue;
use tempo_agentic_vault::EvmSigner;
use tokio::sync::Mutex;

/// How long a broadcast transaction is polled before the call gives up. The
/// receipt keeps landing on chain afterwards; only this call stops waiting.
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(120);
const RECEIPT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Coordinates market research, trade quoting, and execution across trade venues.
#[derive(Clone)]
pub struct AgentService {
    config: Arc<Config>,
    graph: GraphClient,
    venues: Arc<Vec<Arc<dyn TradeVenue>>>,
    chains: Arc<HashMap<u64, Arc<dyn ChainClient>>>,
    signer: Arc<dyn Signer>,
    audit: Arc<dyn AuditStore>,
    quotes: Arc<Mutex<HashMap<String, StoredQuote>>>,
    sequence: Arc<AtomicU64>,
}

impl AgentService {
    /// Returns an error if the graph client, a chain RPC URL, or the keystore
    /// cannot be initialized.
    pub fn new(config: Config, audit: Arc<dyn AuditStore>) -> Result<Self> {
        let graph = GraphClient::new(&config.graph)?;
        let mut chains: HashMap<u64, Arc<dyn ChainClient>> = HashMap::new();
        for chain in &config.evm.chains {
            let client = EvmChainClient::new(&chain.rpc_url, chain.chain_id)
                .with_context(|| format!("cannot build a chain client for {}", chain.name))?;
            chains.insert(chain.chain_id, Arc::new(client));
        }

        // Decrypted once here rather than per transaction, so scrypt never runs
        // on the async runtime mid-trade.
        let signer: Arc<dyn Signer> = Arc::new(EvmSigner::from_keystore(
            Path::new(&config.evm.keystore_path),
            Path::new(&config.evm.password_file),
        )?);

        let mut venues: Vec<Arc<dyn TradeVenue>> = vec![Arc::new(UniswapVenue::new(
            &config.uniswap,
            &config.evm,
            signer.address().to_string(),
            chains.clone(),
            graph.clone(),
            config.max_slippage_bps,
        )?)];
        if config.sui.enabled {
            venues.push(Arc::new(CetusVenue::new(&config.sui)?));
        }
        Ok(Self {
            config: Arc::new(config),
            graph,
            venues: Arc::new(venues),
            chains: Arc::new(chains),
            signer,
            audit,
            quotes: Arc::new(Mutex::new(HashMap::new())),
            sequence: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Queries token market data across requested chains.
    ///
    /// Returns an error if a requested chain is unconfigured or the graph query fails.
    pub async fn market_research(&self, request: MarketResearchRequest) -> Result<MarketResearch> {
        let chains = select_chains(&self.config.evm.chains, &request.chains)?;
        let result = self
            .graph
            .research(&request.token_in, &request.token_out, &chains)
            .await?;
        self.audit.record_research(&request, &result).await?;
        Ok(result)
    }

    /// Generates and stores a time-bound quote for a trade on the requested venue.
    ///
    /// Returns an error if the requested venue is unsupported or quoting fails.
    pub async fn quote_trade(&self, request: QuoteTradeRequest) -> Result<QuoteView> {
        let draft = self.venue(request.venue.as_str())?.quote(&request).await?;
        let created_at_unix = unix_now();
        let expires_at_unix = created_at_unix + self.config.quote_ttl_seconds;
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let id = format!("q-{:x}-{sequence:x}", unix_now_nanos());
        let stored = StoredQuote {
            id: id.clone(),
            expires_at_unix,
            draft,
        };
        let view = stored.view();
        let plan_digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&stored.draft.plan)?)
        );
        self.audit
            .record_quote(&request, &view, &plan_digest)
            .await?;
        let mut quotes = self.quotes.lock().await;
        quotes.retain(|_, quote| !quote.expired());
        quotes.insert(id, stored);
        Ok(view)
    }

    /// Executes a stored quote on its venue and records audit logs.
    ///
    /// Returns an error if the quote is missing, expired, already used, or execution fails.
    pub async fn execute_trade(&self, request: ExecuteTradeRequest) -> Result<ExecutionView> {
        let quote = self
            .quotes
            .lock()
            .await
            .remove(&request.quote_id)
            .with_context(|| {
                format!(
                    "quote {} was not found, expired, or already consumed",
                    request.quote_id
                )
            })?;
        let attempt_id = self.audit.claim_quote(&request, unix_now()).await?;

        // Transactions are collected as they are broadcast, so a failure on a
        // later step still reports the ones that already reached the chain.
        let mut transactions = Vec::new();
        let venue_ref = self.venue(&quote.draft.venue)?;
        let outcome = if quote.draft.venue == "cetus" {
            venue_ref.execute(&quote.draft.plan).await.map(|txs| {
                transactions.extend(txs);
            })
        } else {
            self.run_plan(&quote.draft.venue, &quote.draft.plan, &mut transactions)
                .await
        };
        if let Err(error) = outcome {
            let _ = self.audit.record_execution_failure(attempt_id).await;
            return Err(error);
        }

        let mut result = ExecutionView {
            quote_id: quote.id,
            venue: quote.draft.venue.clone(),
            chain: quote.draft.chain.clone(),
            transactions,
            status: "confirmed".into(),
        };
        if self
            .audit
            .record_execution_success(attempt_id, &result)
            .await
            .is_err()
        {
            result.status = "confirmed_audit_failed".into();
        }
        Ok(result)
    }

    /// Runs a plan's steps, appending a reference for each broadcast transaction.
    ///
    /// Every step is signed, broadcast, and confirmed in turn. `transactions`
    /// accumulates as it goes so the caller keeps what already landed even when
    /// a later step fails.
    async fn run_plan(
        &self,
        venue_name: &str,
        plan: &ExecutionPlan,
        transactions: &mut Vec<TransactionReference>,
    ) -> Result<()> {
        let venue = self.venue(venue_name)?;
        let chain = self.chain_client(plan)?;
        for step in venue.steps(plan).await? {
            let ctx = chain.tx_context(self.signer.address()).await?;
            let unsigned = venue.build(plan, step, &ctx).await?;
            let signed = self.signer.sign(&unsigned).await?;
            let tx_hash = chain.broadcast(&signed).await?;
            transactions.push(TransactionReference {
                kind: step.as_str().to_string(),
                id: tx_hash.clone(),
            });
            await_receipt(chain.as_ref(), &tx_hash).await?;
        }
        Ok(())
    }

    fn chain_client(&self, plan: &ExecutionPlan) -> Result<&Arc<dyn ChainClient>> {
        let ExecutionPlan::Uniswap { chain_id, .. } = plan else {
            bail!("only Uniswap execution plans can be executed");
        };
        self.chains
            .get(chain_id)
            .with_context(|| format!("no chain client configured for chain {chain_id}"))
    }

    fn venue(&self, name: &str) -> Result<&Arc<dyn TradeVenue>> {
        self.venues
            .iter()
            .find(|venue| venue.name() == name)
            .with_context(|| format!("unsupported trade venue {name}"))
    }
}

/// Polls until the transaction is mined, or the timeout expires.
///
/// Returns an error if it reverted or did not appear in time. A timeout does not
/// mean the transaction failed; only that this call stopped waiting.
async fn await_receipt(chain: &dyn ChainClient, tx_hash: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + RECEIPT_TIMEOUT;
    loop {
        match chain.confirmation(tx_hash).await? {
            ReceiptStatus::Success => return Ok(()),
            ReceiptStatus::Reverted => bail!("transaction {tx_hash} reverted on chain"),
            ReceiptStatus::Pending => {}
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("transaction {tx_hash} was not confirmed within {RECEIPT_TIMEOUT:?}");
        }
        tokio::time::sleep(RECEIPT_POLL_INTERVAL).await;
    }
}

fn select_chains<'a>(
    configured: &'a [EvmChain],
    requested: &[String],
) -> Result<Vec<&'a EvmChain>> {
    for value in requested {
        if !configured.iter().any(|chain| {
            value.eq_ignore_ascii_case(&chain.name) || value == &chain.chain_id.to_string()
        }) {
            bail!("requested EVM chain {value} is not configured");
        }
    }
    let selected = configured
        .iter()
        .filter(|chain| {
            requested.is_empty()
                || requested.iter().any(|value| {
                    value.eq_ignore_ascii_case(&chain.name) || value == &chain.chain_id.to_string()
                })
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("none of the requested EVM chains is configured");
    }
    Ok(selected)
}
