use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use tempo_agentic_price::{
    DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_MOVE_BPS, FilteredSource, PricePair, PriceSource,
    PriceStream, PriceTick,
};

const BASE_CHAIN_ID: u64 = 8453;
const WETH: &str = "0x4200000000000000000000000000000000000006";
const USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn weth() -> PricePair {
    PricePair::new(BASE_CHAIN_ID, WETH)
}

/// Scripted quotes keyed by token.
struct Scripted {
    frames: Mutex<Vec<(String, Vec<anyhow::Result<PriceTick>>)>>,
}

impl Scripted {
    fn new(frames: Vec<(&str, Vec<anyhow::Result<PriceTick>>)>) -> Self {
        Self {
            frames: Mutex::new(
                frames
                    .into_iter()
                    .map(|(token, items)| (token.to_string(), items))
                    .collect(),
            ),
        }
    }
}

impl PriceSource for Scripted {
    fn stream(&self, pair: &PricePair) -> PriceStream {
        let mut frames = self.frames.lock().expect("lock poisoned");
        let index = frames
            .iter()
            .position(|(token, _)| token.eq_ignore_ascii_case(&pair.token_address))
            .expect("no script for this pair");
        let items = frames.remove(index).1;
        Box::pin(futures::stream::iter(items))
    }
}

fn tick(pair: &PricePair, price_usd: f64, published_at: i64) -> anyhow::Result<PriceTick> {
    Ok(PriceTick {
        pair: pair.clone(),
        price_usd,
        published_at,
    })
}

async fn collect(source: &FilteredSource<Scripted>, pair: &PricePair) -> Vec<Option<f64>> {
    source
        .stream(pair)
        .map(|item| item.ok().map(|tick| tick.price_usd))
        .collect()
        .await
}

fn filtered(frames: Vec<(&str, Vec<anyhow::Result<PriceTick>>)>) -> FilteredSource<Scripted> {
    FilteredSource::new(
        Scripted::new(frames),
        DEFAULT_MAX_AGE_SECS,
        DEFAULT_MAX_MOVE_BPS,
    )
}

#[tokio::test]
async fn a_fresh_quote_passes() {
    let pair = weth();
    let source = filtered(vec![(WETH, vec![tick(&pair, 1_600.0, now())])]);
    assert_eq!(collect(&source, &pair).await, vec![Some(1_600.0)]);
}

// The first quote has no movement baseline.
#[tokio::test]
async fn the_first_quote_passes_without_a_baseline() {
    let pair = weth();
    let source = filtered(vec![(WETH, vec![tick(&pair, 0.000_001, now())])]);
    assert_eq!(collect(&source, &pair).await, vec![Some(0.000_001)]);
}

#[tokio::test]
async fn a_stale_quote_is_dropped() {
    let pair = weth();
    let source = filtered(vec![(
        WETH,
        vec![
            tick(&pair, 1_600.0, now() - DEFAULT_MAX_AGE_SECS - 60),
            tick(&pair, 1_601.0, now()),
        ],
    )]);
    assert_eq!(collect(&source, &pair).await, vec![Some(1_601.0)]);
}

#[tokio::test]
async fn a_quote_from_the_future_is_dropped() {
    let pair = weth();
    let source = filtered(vec![(
        WETH,
        vec![
            tick(&pair, 1_600.0, now() + 3_600),
            tick(&pair, 1_601.0, now()),
        ],
    )]);
    assert_eq!(collect(&source, &pair).await, vec![Some(1_601.0)]);
}

// A rejected quote must not poison the baseline.
#[tokio::test]
async fn a_wild_jump_is_dropped_without_poisoning_the_baseline() {
    let pair = weth();
    let source = filtered(vec![(
        WETH,
        vec![
            tick(&pair, 1_600.0, now()),
            tick(&pair, 160_000.0, now()),
            tick(&pair, 1_610.0, now()),
        ],
    )]);
    assert_eq!(
        collect(&source, &pair).await,
        vec![Some(1_600.0), Some(1_610.0)]
    );
}

#[tokio::test]
async fn an_ordinary_move_is_not_treated_as_a_jump() {
    let pair = weth();
    let source = filtered(vec![(
        WETH,
        vec![
            tick(&pair, 1_600.0, now()),
            tick(&pair, 1_650.0, now()),
            tick(&pair, 1_700.0, now()),
        ],
    )]);
    assert_eq!(
        collect(&source, &pair).await,
        vec![Some(1_600.0), Some(1_650.0), Some(1_700.0)]
    );
}

// Transient errors must not end the stream.
#[tokio::test]
async fn an_error_is_forwarded_and_the_stream_survives() {
    let pair = weth();
    let source = filtered(vec![(
        WETH,
        vec![
            tick(&pair, 1_600.0, now()),
            Err(anyhow::anyhow!("transport blip")),
            tick(&pair, 1_610.0, now()),
        ],
    )]);
    let items: Vec<_> = source.stream(&pair).collect().await;
    assert_eq!(items.len(), 3);
    assert!(items[0].is_ok());
    assert!(items[1].is_err());
    assert_eq!(items[2].as_ref().unwrap().price_usd, 1_610.0);
}

// Each pair needs an independent baseline.
#[tokio::test]
async fn each_pair_keeps_its_own_baseline() {
    let weth_pair = weth();
    let usdc_pair = PricePair::new(BASE_CHAIN_ID, USDC);
    let source = filtered(vec![
        (WETH, vec![tick(&weth_pair, 1_600.0, now())]),
        (USDC, vec![tick(&usdc_pair, 1.0, now())]),
    ]);

    assert_eq!(collect(&source, &weth_pair).await, vec![Some(1_600.0)]);
    assert_eq!(collect(&source, &usdc_pair).await, vec![Some(1.0)]);
}
