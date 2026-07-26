use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, UniswapConfig};
use tempo_agentic_domain::{
    ChainClient, QuoteTradeRequest, ReceiptStatus, SignedTx, TradeVenue, TxContext,
};
use tempo_agentic_graph::GraphClient;
use tempo_agentic_uniswap::UniswapVenue;

const WALLET: &str = "0x1111111111111111111111111111111111111111";
const TOKEN_IN: &str = "0x2222222222222222222222222222222222222222";
const TOKEN_OUT: &str = "0x3333333333333333333333333333333333333333";
const CHAIN_ID: u64 = 8453;

struct FakeChainClient;

#[async_trait]
impl ChainClient for FakeChainClient {
    fn chain_id(&self) -> u64 {
        CHAIN_ID
    }
    async fn tx_context(&self, _from: &str) -> Result<TxContext> {
        bail!("not used by quote()")
    }
    async fn balance_of(&self, _token: &str, _owner: &str) -> Result<String> {
        Ok("1000000000000000000000".to_string())
    }
    async fn allowance(&self, _token: &str, _owner: &str, _spender: &str) -> Result<String> {
        bail!("not used by quote()")
    }
    async fn estimate_gas(&self, _from: &str, _to: &str, _value: &str, _data: &str) -> Result<u64> {
        bail!("not used by quote()")
    }
    async fn broadcast(&self, _signed: &SignedTx) -> Result<String> {
        bail!("not used by quote()")
    }
    async fn confirmation(&self, _tx_hash: &str) -> Result<ReceiptStatus> {
        bail!("not used by quote()")
    }
}

async fn venue(mock_uri: &str, key_env: &str) -> UniswapVenue {
    let uniswap_config = UniswapConfig {
        api_url: mock_uri.to_string(),
        api_key_env: key_env.to_string(),
    };
    let mut tokens = HashMap::new();
    tokens.insert(
        "IN".to_string(),
        EvmToken {
            address: TOKEN_IN.to_string(),
            decimals: 18,
        },
    );
    tokens.insert(
        "OUT".to_string(),
        EvmToken {
            address: TOKEN_OUT.to_string(),
            decimals: 18,
        },
    );
    let evm = EvmConfig {
        keystore_path: String::new(),
        password_file: String::new(),
        chains: vec![EvmChain {
            name: "base".to_string(),
            chain_id: CHAIN_ID,
            rpc_url: "http://unused.invalid".to_string(),
            // Left empty so the graph research guard is skipped and the mock
            // server never has to serve a /research query.
            graph_subgraph_id: String::new(),
            tokens,
        }],
    };
    let graph_config = tempo_agentic_config::GraphConfig {
        gateway_url: "http://unused.invalid".to_string(),
        api_key_env: key_env.to_string(),
        min_pool_tvl_usd: "0".to_string(),
    };
    let mut chains: HashMap<u64, Arc<dyn ChainClient>> = HashMap::new();
    chains.insert(CHAIN_ID, Arc::new(FakeChainClient));
    // Both constructors read `key_env` synchronously, so the var only needs
    // to exist for the duration of this closure.
    temp_env::with_var(key_env, Some("test-key"), || {
        UniswapVenue::new(
            &uniswap_config,
            &evm,
            WALLET.to_string(),
            chains,
            GraphClient::new(&graph_config).unwrap(),
            500,
        )
        .unwrap()
    })
}

fn request() -> QuoteTradeRequest {
    QuoteTradeRequest {
        venue: tempo_agentic_domain::VenueName::Uniswap,
        token_in: "IN".to_string(),
        token_out: "OUT".to_string(),
        amount: "1".to_string(),
        slippage_bps: 50,
        chains: vec![],
    }
}

// A quote response that passes every validation, so each test can flip one
// field and know that field is the only reason it now fails.
fn valid_quote_response() -> Value {
    json!({
        "routing": "CLASSIC",
        "quote": {
            "chainId": CHAIN_ID,
            "swapper": WALLET,
            "tradeType": "EXACT_INPUT",
            "input": { "token": TOKEN_IN, "amount": "1000000000000000000" },
            "output": {
                "token": TOKEN_OUT,
                "amount": "990000000000000000",
                "minimumAmount": "985050000000000000"
            }
        }
    })
}

async fn mount_quote(server: &MockServer, response: Value) {
    Mock::given(method("POST"))
        .and(path("/quote"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .mount(server)
        .await;
}

#[tokio::test]
async fn accepts_a_fully_valid_quote() {
    let server = MockServer::start().await;
    mount_quote(&server, valid_quote_response()).await;
    let venue = venue(&server.uri(), "UNISWAP_TEST_KEY_VALID").await;
    let draft = venue.quote(&request()).await.unwrap();
    assert_eq!(draft.venue, "uniswap");
    assert_eq!(draft.expected_amount_out, "0.99");
}

#[tokio::test]
async fn rejects_chain_mismatch() {
    let server = MockServer::start().await;
    let mut response = valid_quote_response();
    response["quote"]["chainId"] = json!(1);
    mount_quote(&server, response).await;
    let venue = venue(&server.uri(), "UNISWAP_TEST_KEY_CHAIN").await;
    let error = venue.quote(&request()).await.unwrap_err();
    assert!(
        error.to_string().contains("unexpected chain"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn rejects_sender_mismatch() {
    let server = MockServer::start().await;
    let mut response = valid_quote_response();
    response["quote"]["swapper"] = json!("0x9999999999999999999999999999999999999999");
    mount_quote(&server, response).await;
    let venue = venue(&server.uri(), "UNISWAP_TEST_KEY_SENDER").await;
    let error = venue.quote(&request()).await.unwrap_err();
    assert!(
        error.to_string().contains("unexpected swapper"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn rejects_non_classic_routing() {
    let server = MockServer::start().await;
    let mut response = valid_quote_response();
    response["routing"] = json!("DUTCH_LIMIT");
    mount_quote(&server, response).await;
    let venue = venue(&server.uri(), "UNISWAP_TEST_KEY_ROUTING").await;
    let error = venue.quote(&request()).await.unwrap_err();
    assert!(
        error.to_string().contains("rejected Uniswap route"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn rejects_malformed_output_amount() {
    let server = MockServer::start().await;
    let mut response = valid_quote_response();
    response["quote"]["output"]["amount"] = json!("not-a-uint256");
    mount_quote(&server, response).await;
    let venue = venue(&server.uri(), "UNISWAP_TEST_KEY_UINT").await;
    let error = venue.quote(&request()).await.unwrap_err();
    assert!(
        error.to_string().contains("invalid Uniswap output amount"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn rejects_minimum_output_below_the_requested_slippage_floor() {
    let server = MockServer::start().await;
    let mut response = valid_quote_response();
    // 50 bps of slippage on 990000000000000000 floors at 985050000000000000;
    // this is below that floor.
    response["quote"]["output"]["minimumAmount"] = json!("100000000000000000");
    mount_quote(&server, response).await;
    let venue = venue(&server.uri(), "UNISWAP_TEST_KEY_SLIPPAGE").await;
    let error = venue.quote(&request()).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("below the requested slippage floor"),
        "unexpected error: {error}"
    );
}

// The approval step is where the API-controlled spender is checked: build()
// refuses to sign an approval that grants allowance to anything but the
// venue's known proxy address.
#[tokio::test]
async fn rejects_approval_calldata_to_an_unexpected_spender() {
    let server = MockServer::start().await;
    mount_quote(&server, valid_quote_response()).await;
    let wrong_spender = "9999999999999999999999999999999999999999";
    let approval_data = format!(
        "0x095ea7b3000000000000000000000000{wrong_spender}0000000000000000000000000000000000000000000000000000000000000001"
    );
    Mock::given(method("POST"))
        .and(path("/check_approval"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "approval": {
                "from": WALLET,
                "to": TOKEN_IN,
                "data": approval_data,
                "chainId": CHAIN_ID,
                "value": "0"
            }
        })))
        .mount(&server)
        .await;

    let venue = venue(&server.uri(), "UNISWAP_TEST_KEY_SPENDER").await;
    let draft = venue.quote(&request()).await.unwrap();
    let ctx = TxContext {
        chain_id: CHAIN_ID,
        nonce: 0,
        max_fee_per_gas: 1,
        max_priority_fee_per_gas: 1,
    };
    let error = venue
        .build(&draft.plan, tempo_agentic_domain::ExecStep::Approval, &ctx)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("unexpected spender"),
        "unexpected error: {error}"
    );
}
