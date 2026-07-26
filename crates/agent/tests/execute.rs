use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use async_trait::async_trait;
use serde_json::json;

use tempo_agentic_agent::AgentService;
use tempo_agentic_config::{
    Config, EvmChain, EvmConfig, GraphConfig, SuiConfig, SuiNetwork, UniswapConfig,
};
use tempo_agentic_domain::{
    AuditStore, ChainClient, ExecStep, ExecuteTradeRequest, ExecutionPlan, ExecutionView,
    MarketResearch, MarketResearchRequest, QuoteDraft, QuoteTradeRequest, QuoteView, ReceiptStatus,
    SignedTx, Signer, TradeVenue, TxContext, UnsignedTx, VenueName,
};
use tempo_agentic_graph::GraphClient;

const WALLET: &str = "0x1111111111111111111111111111111111111111";
const CHAIN_ID: u64 = 8453;

#[derive(Default)]
struct Calls(Mutex<Vec<String>>);

impl Calls {
    fn record(&self, call: &str) {
        self.0.lock().unwrap().push(call.to_string());
    }
    fn snapshot(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

struct RecordingVenue {
    calls: Arc<Calls>,
}

#[async_trait]
impl TradeVenue for RecordingVenue {
    fn name(&self) -> &'static str {
        "uniswap"
    }

    async fn quote(&self, _request: &QuoteTradeRequest) -> Result<QuoteDraft> {
        self.calls.record("quote");
        Ok(QuoteDraft {
            venue: "uniswap".into(),
            chain: "base".into(),
            token_in: "IN".into(),
            token_out: "OUT".into(),
            amount_in: "1".into(),
            expected_amount_out: "1".into(),
            minimum_amount_out: "1".into(),
            graph_guard: "skipped".into(),
            plan: ExecutionPlan::Uniswap {
                chain_name: "base".into(),
                chain_id: CHAIN_ID,
                input_token: "0x2222222222222222222222222222222222222222".into(),
                input_amount: "1000000000000000000".into(),
                quote: json!({}),
            },
        })
    }

    async fn steps(&self, _plan: &ExecutionPlan) -> Result<Vec<ExecStep>> {
        self.calls.record("steps");
        Ok(vec![ExecStep::Swap])
    }

    async fn build(
        &self,
        _plan: &ExecutionPlan,
        step: ExecStep,
        _ctx: &TxContext,
    ) -> Result<UnsignedTx> {
        self.calls.record("build");
        assert_eq!(step, ExecStep::Swap);
        Ok(UnsignedTx {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit: 21_000,
            max_fee_per_gas: 1,
            max_priority_fee_per_gas: 1,
            to: "0x3333333333333333333333333333333333333333".into(),
            value: "0".into(),
            data: "0x".into(),
        })
    }
}

struct RecordingChain {
    calls: Arc<Calls>,
}

#[async_trait]
impl ChainClient for RecordingChain {
    fn chain_id(&self) -> u64 {
        CHAIN_ID
    }
    async fn tx_context(&self, _from: &str) -> Result<TxContext> {
        self.calls.record("tx_context");
        Ok(TxContext {
            chain_id: CHAIN_ID,
            nonce: 0,
            max_fee_per_gas: 1,
            max_priority_fee_per_gas: 1,
        })
    }
    async fn balance_of(&self, _token: &str, _owner: &str) -> Result<String> {
        bail!("not used by execute_trade")
    }
    async fn allowance(&self, _token: &str, _owner: &str, _spender: &str) -> Result<String> {
        bail!("not used by execute_trade")
    }
    async fn estimate_gas(&self, _from: &str, _to: &str, _value: &str, _data: &str) -> Result<u64> {
        bail!("not used by execute_trade")
    }
    async fn broadcast(&self, _signed: &SignedTx) -> Result<String> {
        self.calls.record("broadcast");
        Ok("0xdeadbeef".to_string())
    }
    async fn confirmation(&self, _tx_hash: &str) -> Result<ReceiptStatus> {
        self.calls.record("confirmation");
        Ok(ReceiptStatus::Success)
    }
}

struct RecordingSigner {
    calls: Arc<Calls>,
}

#[async_trait]
impl Signer for RecordingSigner {
    fn address(&self) -> &str {
        WALLET
    }
    async fn sign(&self, _tx: &UnsignedTx) -> Result<SignedTx> {
        self.calls.record("sign");
        Ok(SignedTx {
            raw: "0xsigned".into(),
            hash: "0xdeadbeef".into(),
        })
    }
}

struct NoopAuditStore;

#[async_trait]
impl AuditStore for NoopAuditStore {
    fn session_id(&self) -> &str {
        "test-session"
    }
    async fn record_research(
        &self,
        _r: &MarketResearchRequest,
        _res: &MarketResearch,
    ) -> Result<()> {
        Ok(())
    }
    async fn record_quote(
        &self,
        _r: &QuoteTradeRequest,
        _q: &QuoteView,
        _digest: &str,
    ) -> Result<()> {
        Ok(())
    }
    async fn claim_quote(&self, _r: &ExecuteTradeRequest, _now: u64) -> Result<i64> {
        Ok(1)
    }
    async fn record_execution_success(&self, _attempt_id: i64, _r: &ExecutionView) -> Result<()> {
        Ok(())
    }
    async fn record_execution_failure(&self, _attempt_id: i64) -> Result<()> {
        Ok(())
    }
}

fn config() -> Config {
    Config {
        state_db_path: "/tmp/unused.db".into(),
        quote_ttl_seconds: 60,
        max_slippage_bps: 500,
        dexpaprika_stream_url: "http://unused.invalid".into(),
        uniswap: UniswapConfig {
            api_url: "http://unused.invalid".into(),
            api_key_env: "UNUSED_UNISWAP_KEY".into(),
        },
        graph: GraphConfig {
            gateway_url: "http://unused.invalid".into(),
            api_key_env: "UNUSED_GRAPH_KEY".into(),
            min_pool_tvl_usd: "0".into(),
        },
        evm: EvmConfig {
            keystore_path: String::new(),
            password_file: String::new(),
            chains: vec![EvmChain {
                name: "base".into(),
                chain_id: CHAIN_ID,
                rpc_url: "http://unused.invalid".into(),
                graph_subgraph_id: String::new(),
                tokens: HashMap::new(),
            }],
        },
        sui: SuiConfig {
            enabled: false,
            network: SuiNetwork::Testnet,
            rpc_url: "http://unused.invalid".into(),
            keystore_path: None,
        },
    }
}

#[tokio::test]
async fn uniswap_plan_executes_in_order_steps_ctx_build_sign_broadcast_confirmation() {
    let calls = Arc::new(Calls::default());
    let venue: Arc<dyn TradeVenue> = Arc::new(RecordingVenue {
        calls: calls.clone(),
    });
    let chain: Arc<dyn ChainClient> = Arc::new(RecordingChain {
        calls: calls.clone(),
    });
    let signer: Arc<dyn Signer> = Arc::new(RecordingSigner {
        calls: calls.clone(),
    });
    let mut chains = HashMap::new();
    chains.insert(CHAIN_ID, chain);

    let graph_config = GraphConfig {
        gateway_url: "http://unused.invalid".into(),
        api_key_env: "UNUSED_EXECUTE_GRAPH_KEY".into(),
        min_pool_tvl_usd: "0".into(),
    };
    let graph = temp_env::with_var("UNUSED_EXECUTE_GRAPH_KEY", Some("unused"), || {
        GraphClient::new(&graph_config).unwrap()
    });

    let service = AgentService::from_parts(
        config(),
        graph,
        vec![venue],
        chains,
        signer,
        Arc::new(NoopAuditStore),
    );

    let quote = service
        .quote_trade(QuoteTradeRequest {
            venue: VenueName::Uniswap,
            token_in: "IN".into(),
            token_out: "OUT".into(),
            amount: "1".into(),
            slippage_bps: 50,
            chains: vec![],
        })
        .await
        .unwrap();

    let result = service
        .execute_trade(ExecuteTradeRequest {
            quote_id: quote.quote_id,
            confirmed: true,
        })
        .await
        .unwrap();

    assert_eq!(result.status, "confirmed");
    assert_eq!(result.transactions.len(), 1);
    assert_eq!(
        calls.snapshot(),
        vec![
            "quote",
            "steps",
            "tx_context",
            "build",
            "sign",
            "broadcast",
            "confirmation"
        ]
    );
}
