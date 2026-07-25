// Cargo compiles this module into every test binary that declares it, so
// whatever one binary does not call looks unused to the compiler.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::U256;
use anyhow::{Result, bail};
use async_trait::async_trait;
use tempo_agentic_domain::{
    ChainClient, ExecStep, ExecutionPlan, QuoteDraft, QuoteTradeRequest, ReceiptStatus, SignedTx,
    Signer, TradeVenue, TxContext, UnsignedTx, VenueName,
};
use tempo_agentic_orchestrator::{ExecDeps, drive_order, sweep};
use tempo_agentic_storage::{SqliteLevelStore, SqliteOrderStore, connect_pool};
use tempo_agentic_strategy::{Level, LevelStore, Order, OrderState, OrderStore, Side};

pub const BASE_ID: u64 = 8453;
const USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const WALLET: &str = "0x1111111111111111111111111111111111111111";

pub fn phase(state: &OrderState) -> &'static str {
    match state {
        OrderState::Withdrawing { .. } => "withdrawing",
        OrderState::SwapReady { .. } => "swap_ready",
        OrderState::Broadcasting { .. } => "broadcasting",
        OrderState::Submitted { .. } => "submitted",
        OrderState::Depositing { .. } => "depositing",
        OrderState::Filled { .. } => "filled",
        OrderState::Failed { .. } => "failed",
        OrderState::SwapQuarantined { .. } => "quarantined",
    }
}

pub fn level() -> Level {
    Level {
        id: "l-1".into(),
        venue: VenueName::Uniswap,
        chain: "base".into(),
        token_in: "USDC".into(),
        token_out: "WETH".into(),
        side: Side::Buy,
        trigger_price_usd: 3_000.0,
        amount: U256::from(1_000_000u64),
        amount_decimals: 6,
        slippage_bps: 50,
    }
}

pub fn order(id: &str, chain_id: u64) -> Order {
    let plan = ExecutionPlan::Uniswap {
        chain_name: "base".into(),
        chain_id,
        input_token: USDC.into(),
        input_amount: "1000000".into(),
        quote: serde_json::json!({"tradeType": "EXACT_INPUT"}),
    };
    Order::new(id.into(), &level(), plan, 0)
}

/// Reads the stored row at the moment a fake is called.
///
/// This is what proves the crash-safety property: the phase seen here is the one
/// that was already durable when the side effect ran, not the one it produced.
#[derive(Clone)]
struct Spy {
    orders: Arc<SqliteOrderStore>,
    order_id: String,
    seen: Arc<Mutex<Vec<&'static str>>>,
}

impl Spy {
    async fn record(&self) {
        let stored = self.orders.get_order(&self.order_id).await.unwrap();
        let phase = stored
            .as_ref()
            .map_or("missing", |order| phase(&order.state));
        self.seen.lock().unwrap().push(phase);
    }
}

#[derive(Clone, Copy)]
pub enum Receipt {
    Pending,
    Success,
    Reverted,
    /// The node could not answer at all.
    Error,
}

/// Reads the entry for this call, repeating the last one once the script runs out.
fn pick<T: Clone>(script: &[T], cursor: &AtomicUsize) -> T {
    let index = cursor.fetch_add(1, Ordering::SeqCst);
    script[index.min(script.len() - 1)].clone()
}

struct FakeVenue {
    steps: Vec<Vec<ExecStep>>,
    steps_calls: AtomicUsize,
    build_fails: bool,
}

#[async_trait]
impl TradeVenue for FakeVenue {
    fn name(&self) -> &'static str {
        "uniswap"
    }

    async fn quote(&self, _request: &QuoteTradeRequest) -> Result<QuoteDraft> {
        bail!("the orchestrator never quotes")
    }

    async fn steps(&self, _plan: &ExecutionPlan) -> Result<Vec<ExecStep>> {
        Ok(pick(&self.steps, &self.steps_calls))
    }

    async fn build(
        &self,
        _plan: &ExecutionPlan,
        _step: ExecStep,
        ctx: &TxContext,
    ) -> Result<UnsignedTx> {
        if self.build_fails {
            bail!("Uniswap /swap returned 400: quote expired");
        }
        Ok(UnsignedTx {
            chain_id: ctx.chain_id,
            nonce: ctx.nonce,
            gas_limit: 21_000,
            max_fee_per_gas: ctx.max_fee_per_gas,
            max_priority_fee_per_gas: ctx.max_priority_fee_per_gas,
            to: "0x2222222222222222222222222222222222222222".into(),
            value: "0".into(),
            data: "0xdeadbeef".into(),
        })
    }
}

struct FakeSigner {
    spy: Spy,
    signatures: AtomicUsize,
}

#[async_trait]
impl Signer for FakeSigner {
    fn address(&self) -> &str {
        WALLET
    }

    async fn sign(&self, _tx: &UnsignedTx) -> Result<SignedTx> {
        self.spy.record().await;
        let nth = self.signatures.fetch_add(1, Ordering::SeqCst);
        Ok(SignedTx {
            raw: format!("0xraw{nth}"),
            hash: format!("0xhash{nth}"),
        })
    }
}

struct FakeChain {
    spy: Spy,
    receipts: Vec<Receipt>,
    receipt_calls: AtomicUsize,
    broadcasts: AtomicUsize,
    /// Raw bytes of every send, so a test can prove a resumed order reused the
    /// signature it already had instead of making a new one.
    sent: Mutex<Vec<String>>,
    broadcast_fails: bool,
}

#[async_trait]
impl ChainClient for FakeChain {
    fn chain_id(&self) -> u64 {
        BASE_ID
    }

    async fn tx_context(&self, _from: &str) -> Result<TxContext> {
        Ok(TxContext {
            chain_id: BASE_ID,
            nonce: 7,
            max_fee_per_gas: 1_000,
            max_priority_fee_per_gas: 1,
        })
    }

    async fn balance_of(&self, _token: &str, _owner: &str) -> Result<String> {
        bail!("the orchestrator never reads balances")
    }

    async fn allowance(&self, _token: &str, _owner: &str, _spender: &str) -> Result<String> {
        bail!("the orchestrator never reads allowances")
    }

    async fn estimate_gas(&self, _from: &str, _to: &str, _value: &str, _data: &str) -> Result<u64> {
        bail!("the venue estimates gas, not the orchestrator")
    }

    async fn broadcast(&self, signed: &SignedTx) -> Result<String> {
        self.spy.record().await;
        self.broadcasts.fetch_add(1, Ordering::SeqCst);
        self.sent.lock().unwrap().push(signed.raw.clone());
        if self.broadcast_fails {
            bail!("eth_sendRawTransaction failed: insufficient funds for gas");
        }
        Ok(signed.hash.clone())
    }

    async fn confirmation(&self, _tx_hash: &str) -> Result<ReceiptStatus> {
        self.spy.record().await;
        match pick(&self.receipts, &self.receipt_calls) {
            Receipt::Pending => Ok(ReceiptStatus::Pending),
            Receipt::Success => Ok(ReceiptStatus::Success),
            Receipt::Reverted => Ok(ReceiptStatus::Reverted),
            Receipt::Error => bail!("eth_getTransactionReceipt failed: connection reset"),
        }
    }
}

/// What the fakes answer. Each script repeats its last entry once exhausted, so
/// a test only spells out the calls it actually cares about.
pub struct Script {
    pub steps: Vec<Vec<ExecStep>>,
    pub receipts: Vec<Receipt>,
    pub build_fails: bool,
    pub broadcast_fails: bool,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            steps: vec![vec![ExecStep::Swap]],
            receipts: vec![Receipt::Success],
            build_fails: false,
            broadcast_fails: false,
        }
    }
}

pub struct Harness {
    levels: Arc<SqliteLevelStore>,
    orders: Arc<SqliteOrderStore>,
    deps: ExecDeps,
    seen: Arc<Mutex<Vec<&'static str>>>,
    venue: Arc<FakeVenue>,
    chain: Arc<FakeChain>,
    signer: Arc<FakeSigner>,
    path: PathBuf,
}

impl Harness {
    /// Builds a database holding one order, `o-1`, in `SwapReady`, with the fakes
    /// watching that row.
    pub async fn new(name: &str, script: Script) -> Self {
        let path = std::env::temp_dir().join(format!(
            "tempo-agentic-orchestrator-{name}-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let harness = Self::open(path, script).await;
        harness.levels.upsert_level(&level()).await.unwrap();
        harness.put(&order("o-1", BASE_ID)).await;
        harness
    }

    /// Closes the database and opens the very same file again with fresh fakes.
    ///
    /// This stands in for a restart, so nothing is seeded: the level and the
    /// order are already on disk. Migrations are idempotent, so replaying them
    /// on reopen is safe.
    pub async fn reopen(self, script: Script) -> Self {
        let path = self.path.clone();
        drop(self);
        Self::open(path, script).await
    }

    async fn open(path: PathBuf, script: Script) -> Self {
        let pool = connect_pool(&path).await.unwrap();
        let levels = Arc::new(SqliteLevelStore::new(pool.clone()));
        let orders = Arc::new(SqliteOrderStore::new(pool));

        let seen = Arc::new(Mutex::new(Vec::new()));
        let spy = Spy {
            orders: orders.clone(),
            order_id: "o-1".into(),
            seen: seen.clone(),
        };
        let venue = Arc::new(FakeVenue {
            steps: script.steps,
            steps_calls: AtomicUsize::new(0),
            build_fails: script.build_fails,
        });
        let chain = Arc::new(FakeChain {
            spy: spy.clone(),
            receipts: script.receipts,
            receipt_calls: AtomicUsize::new(0),
            broadcasts: AtomicUsize::new(0),
            sent: Mutex::new(Vec::new()),
            broadcast_fails: script.broadcast_fails,
        });
        let signer = Arc::new(FakeSigner {
            spy,
            signatures: AtomicUsize::new(0),
        });
        let deps = ExecDeps {
            venues: vec![venue.clone()],
            chains: HashMap::from([(BASE_ID, chain.clone() as Arc<dyn ChainClient>)]),
            signer: signer.clone(),
        };
        Self {
            levels,
            orders,
            deps,
            seen,
            venue,
            chain,
            signer,
            path,
        }
    }

    pub async fn drive(&self, id: &str) -> Order {
        let mut order = self.stored(id).await;
        drive_order(&self.deps, self.orders.as_ref(), &mut order)
            .await
            .unwrap();
        order
    }

    pub async fn sweep(&self) {
        sweep(&self.deps, self.orders.as_ref()).await.unwrap();
    }

    pub async fn stored(&self, id: &str) -> Order {
        self.orders.get_order(id).await.unwrap().unwrap()
    }

    pub async fn put(&self, order: &Order) {
        self.orders.upsert_order(order).await.unwrap();
    }

    /// The phases the fakes found on disk, in call order.
    pub fn seen(&self) -> Vec<&'static str> {
        self.seen.lock().unwrap().clone()
    }

    pub fn broadcasts(&self) -> usize {
        self.chain.broadcasts.load(Ordering::SeqCst)
    }

    pub fn steps_calls(&self) -> usize {
        self.venue.steps_calls.load(Ordering::SeqCst)
    }

    pub fn signatures(&self) -> usize {
        self.signer.signatures.load(Ordering::SeqCst)
    }

    /// Raw bytes of every broadcast, in call order.
    pub fn sent(&self) -> Vec<String> {
        self.chain.sent.lock().unwrap().clone()
    }

    pub fn cleanup(self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}
