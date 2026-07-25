use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use tempo_agentic_config::{Config, EvmChain};

use tempo_agentic_cetus::CetusVenue;
use tempo_agentic_domain::{
    AuditStore, ExecuteTradeRequest, ExecutionView, MarketResearch, MarketResearchRequest,
    QuoteTradeRequest, QuoteView, StoredQuote, TradeVenue, unix_now, unix_now_nanos,
};
use tempo_agentic_graph::GraphClient;
use tempo_agentic_uniswap::UniswapVenue;
use tokio::sync::Mutex;

/// Coordinates market research, trade quoting, and execution across trade venues.
#[derive(Clone)]
pub struct AgentService {
    config: Arc<Config>,
    graph: GraphClient,
    venues: Arc<Vec<Arc<dyn TradeVenue>>>,
    audit: Arc<dyn AuditStore>,
    quotes: Arc<Mutex<HashMap<String, StoredQuote>>>,
    sequence: Arc<AtomicU64>,
}

impl AgentService {
    pub fn new(config: Config, audit: Arc<dyn AuditStore>) -> Result<Self> {
        let graph = GraphClient::new(&config.graph)?;
        let mut venues: Vec<Arc<dyn TradeVenue>> = vec![Arc::new(UniswapVenue::new(
            &config.uniswap,
            &config.evm,
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
        let execution = self
            .venue(&quote.draft.venue)?
            .execute(&quote.draft.plan)
            .await;
        let execution = match execution {
            Ok(execution) => execution,
            Err(error) => {
                let _ = self.audit.record_execution_failure(attempt_id).await;
                return Err(error);
            }
        };
        let mut result = ExecutionView {
            quote_id: quote.id,
            venue: execution.venue,
            chain: execution.chain,
            transactions: execution.transactions,
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

    fn venue(&self, name: &str) -> Result<&Arc<dyn TradeVenue>> {
        self.venues
            .iter()
            .find(|venue| venue.name() == name)
            .with_context(|| format!("unsupported trade venue {name}"))
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
