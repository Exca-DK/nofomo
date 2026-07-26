use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::U256;
use anyhow::{Result, bail};
use async_trait::async_trait;
use sui_sdk_types::{
    Address, Digest, GasPayment, ObjectReference, ProgrammableTransaction, Transaction,
    TransactionExpiration, TransactionKind,
};
use tempo_agentic_domain::{
    ChainClient, ChainId, DryRun, ExecStep, ExecutionPlan, QuoteDraft, QuoteTradeRequest,
    ReceiptStatus, SignedTx, Signer, TradeVenue, TxContext, UnsignedTx, VenueName,
};
use tempo_agentic_orchestrator::{ExecDeps, drive_order};
use tempo_agentic_storage::{
    LockFile, SqliteLevelStore, SqliteOrderStore, SqliteStrategyStore, initialize_new_under_lock,
};
use tempo_agentic_strategy::{
    Level, LevelStore, Order, OrderState, OrderStore, Side, Strategy, StrategyLevel, StrategyStore,
};
use tempo_agentic_vault::{Vault, VaultSigner};

const GAS_PRICE: u64 = 1_000;
const GAS_BUDGET: u64 = 100_000_000;

fn plan() -> ExecutionPlan {
    ExecutionPlan::Cetus {
        pool_id: "0x9".into(),
        a2b: true,
        input_amount: 1_000_000,
        min_amount_out: 990_000,
    }
}

fn strategy() -> Strategy {
    Strategy {
        id: "s-sui".into(),
        venue: VenueName::Cetus,
        chain: "sui".into(),
        base_token: "0xfce::btc::BTC".into(),
        quote_token: "0x2::sui::SUI".into(),
    }
}

fn level() -> StrategyLevel {
    StrategyLevel {
        strategy: strategy(),
        level: Level {
            id: "l-sui".into(),
            strategy_id: "s-sui".into(),
            side: Side::Sell,
            trigger_price_usd: 1.0,
            amount: U256::from(1_000_000u64),
            amount_decimals: 8,
            slippage_bps: 50,
        },
    }
}

fn transaction_from(sender: Address) -> Transaction {
    Transaction {
        kind: TransactionKind::ProgrammableTransaction(ProgrammableTransaction {
            inputs: vec![],
            commands: vec![],
        }),
        sender,
        gas_payment: GasPayment {
            objects: vec![ObjectReference::new(
                Address::new([7; 32]),
                3,
                Digest::new([9; 32]),
            )],
            owner: sender,
            price: GAS_PRICE,
            budget: GAS_BUDGET,
        },
        expiration: TransactionExpiration::None,
    }
}

struct FakeCetus {
    sender: Address,
    contexts: Mutex<Vec<TxContext>>,
}

#[async_trait]
impl TradeVenue for FakeCetus {
    fn name(&self) -> &'static str {
        "cetus"
    }

    async fn quote(&self, _request: &QuoteTradeRequest) -> Result<QuoteDraft> {
        bail!("the orchestrator never quotes")
    }

    async fn steps(&self, _plan: &ExecutionPlan) -> Result<Vec<ExecStep>> {
        Ok(vec![ExecStep::Swap])
    }

    async fn build(
        &self,
        _plan: &ExecutionPlan,
        _step: ExecStep,
        ctx: &TxContext,
    ) -> Result<UnsignedTx> {
        self.contexts.lock().unwrap().push(ctx.clone());
        Ok(UnsignedTx::Sui(Box::new(transaction_from(self.sender))))
    }
}

struct FakeSuiNode {
    receipts: Vec<ReceiptStatus>,
    receipt_calls: AtomicUsize,
    sent: Mutex<Vec<String>>,
    simulated: Mutex<Vec<String>>,
    simulation: DryRun,
}

#[async_trait]
impl ChainClient for FakeSuiNode {
    fn chain(&self) -> ChainId {
        ChainId::Sui
    }

    async fn tx_context(&self, _from: &str) -> Result<TxContext> {
        Ok(TxContext::Sui {
            gas_price: GAS_PRICE,
            gas_budget: GAS_BUDGET,
        })
    }

    async fn broadcast(&self, signed: &SignedTx) -> Result<String> {
        let SignedTx::Sui(signed) = signed else {
            bail!("a Sui node was handed another family's transaction");
        };
        self.sent.lock().unwrap().push(signed.digest());
        Ok(signed.digest())
    }

    async fn confirmation(&self, _tx_hash: &str) -> Result<ReceiptStatus> {
        let index = self.receipt_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.receipts[index.min(self.receipts.len() - 1)])
    }

    async fn dry_run(&self, signed: &SignedTx) -> Result<DryRun> {
        let SignedTx::Sui(signed) = signed else {
            bail!("a Sui node was handed another family's transaction");
        };
        self.simulated.lock().unwrap().push(signed.digest());
        Ok(self.simulation.clone())
    }
}

struct Fixture {
    orders: Arc<SqliteOrderStore>,
    deps: ExecDeps,
    node: Arc<FakeSuiNode>,
    venue: Arc<FakeCetus>,
    signer: Arc<dyn Signer>,
    sender: Address,
    path: std::path::PathBuf,
}

impl Fixture {
    async fn new(name: &str, receipts: Vec<ReceiptStatus>) -> Self {
        Self::with_simulation(name, receipts, true, DryRun::Succeeded).await
    }

    async fn with_simulation(
        name: &str,
        receipts: Vec<ReceiptStatus>,
        allow_broadcast: bool,
        simulation: DryRun,
    ) -> Self {
        let path = std::env::temp_dir().join(format!(
            "tempo-agentic-orchestrator-sui-{name}-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // A fresh path needs the schema written under the lock, as `run` does.
        let lock = LockFile::acquire(LockFile::path_for(&path)).unwrap();
        let pool = initialize_new_under_lock(&path, &lock).await.unwrap();
        let strategies = SqliteStrategyStore::new(pool.clone());
        let levels = SqliteLevelStore::new(pool.clone());
        let orders = Arc::new(SqliteOrderStore::new(pool));
        // An order is only valid against a stored strategy and level.
        strategies.upsert_strategy(&strategy()).await.unwrap();
        levels
            .upsert_level(&level().level, &strategy())
            .await
            .unwrap();

        let mut vault = Vault::new();
        vault.add(VaultSigner::generate(
            tempo_agentic_domain::ChainFamily::Sui,
        ));
        let signer: Arc<dyn Signer> = Arc::new(vault);
        let sender = signer
            .address(tempo_agentic_domain::ChainFamily::Sui)
            .unwrap()
            .parse()
            .expect("the vault's address parses");

        let venue = Arc::new(FakeCetus {
            sender,
            contexts: Mutex::new(Vec::new()),
        });
        let node = Arc::new(FakeSuiNode {
            receipts,
            receipt_calls: AtomicUsize::new(0),
            sent: Mutex::new(Vec::new()),
            simulated: Mutex::new(Vec::new()),
            simulation: simulation.clone(),
        });

        let deps = ExecDeps {
            venues: vec![venue.clone()],
            chains: HashMap::from([(ChainId::Sui, node.clone() as Arc<dyn ChainClient>)]),
            signer: signer.clone(),
            allow_broadcast,
        };

        let fixture = Self {
            orders,
            deps,
            node,
            venue,
            signer,
            sender,
            path,
        };
        fixture
            .orders
            .upsert_order(&Order::new("o-sui".into(), &level(), plan(), 0))
            .await
            .unwrap();
        fixture
    }

    async fn drive(&self) -> Order {
        let mut order = self.orders.get_order("o-sui").await.unwrap().unwrap();
        drive_order(&self.deps, self.orders.as_ref(), &mut order)
            .await
            .unwrap();
        order
    }

    fn cleanup(self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

#[tokio::test]
async fn a_sui_order_signs_broadcasts_and_fills() {
    let fixture = Fixture::new("happy", vec![ReceiptStatus::Success]).await;

    let order = fixture.drive().await;

    let OrderState::Filled { tx_hash } = &order.state else {
        panic!(
            "a confirmed Sui order must be filled, got {:?}",
            order.state
        );
    };
    // Exactly one transaction reached the node, under the digest the order kept.
    assert_eq!(
        fixture.node.sent.lock().unwrap().as_slice(),
        std::slice::from_ref(tx_hash)
    );

    fixture.cleanup();
}

#[tokio::test]
async fn a_restart_rebroadcasts_the_transaction_it_signed_before() {
    let fixture = Fixture::new("restart", vec![ReceiptStatus::Success]).await;

    // Simulate state persisted by a previous process.
    let signed = fixture
        .signer
        .sign(&UnsignedTx::Sui(Box::new(transaction_from(fixture.sender))))
        .await
        .expect("sign");
    let digest = signed.hash();

    let mut order = Order::new("o-sui".into(), &level(), plan(), 0);
    order.state = OrderState::Broadcasting {
        step: ExecStep::Swap,
        amount_in: U256::from(1_000_000u64),
        signed_tx: signed.to_wire().expect("encode"),
        tx_hash: digest.clone(),
        withdraw_action_id: None,
    };
    fixture.orders.upsert_order(&order).await.unwrap();

    fixture.drive().await;

    assert_eq!(fixture.node.sent.lock().unwrap().as_slice(), &[digest]);

    fixture.cleanup();
}

#[tokio::test]
async fn the_venue_is_handed_sui_chain_state() {
    let fixture = Fixture::new("context", vec![ReceiptStatus::Pending]).await;
    fixture.drive().await;

    let contexts = fixture.venue.contexts.lock().unwrap().clone();
    assert_eq!(
        contexts,
        vec![TxContext::Sui {
            gas_price: GAS_PRICE,
            gas_budget: GAS_BUDGET,
        }]
    );

    fixture.cleanup();
}

// A run that spends nothing is the only chance to learn whether the transaction
// would have worked, so it has to ask the node and record the answer.
#[tokio::test]
async fn a_blocked_run_dry_runs_and_keeps_what_the_node_said() {
    let fixture = Fixture::with_simulation(
        "dry-run-ok",
        vec![ReceiptStatus::Success],
        false,
        DryRun::Succeeded,
    )
    .await;

    let order = fixture.drive().await;

    assert_eq!(fixture.node.simulated.lock().unwrap().len(), 1);
    assert!(
        fixture.node.sent.lock().unwrap().is_empty(),
        "a blocked run must send nothing"
    );
    let OrderState::Failed { reason, tx_hash } = &order.state else {
        panic!("a blocked broadcast ends the order: {:?}", order.state);
    };
    assert_eq!(tx_hash.as_deref(), None);
    assert!(reason.contains("dry run succeeded"), "{reason}");
    fixture.cleanup();
}

#[tokio::test]
async fn a_dry_run_the_node_rejects_says_why() {
    let fixture = Fixture::with_simulation(
        "dry-run-bad",
        vec![ReceiptStatus::Success],
        false,
        DryRun::Failed("InsufficientCoinBalance".into()),
    )
    .await;

    let order = fixture.drive().await;

    let OrderState::Failed { reason, .. } = &order.state else {
        panic!("a blocked broadcast ends the order: {:?}", order.state);
    };
    assert!(reason.contains("InsufficientCoinBalance"), "{reason}");
    fixture.cleanup();
}
