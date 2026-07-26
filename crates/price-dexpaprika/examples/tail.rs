// Run: cargo run -p tempo-agentic-price-dexpaprika --example tail

use futures::StreamExt;
use tempo_agentic_price::{
    DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_MOVE_BPS, FilteredSource, PricePair, PriceSource,
};
use tempo_agentic_price_dexpaprika::DexPaprikaSource;

const BASE_CHAIN_ID: u64 = 8453;
const WETH_ON_BASE: &str = "0x4200000000000000000000000000000000000006";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let pair = PricePair::new(BASE_CHAIN_ID, WETH_ON_BASE);

    let source = FilteredSource::new(
        DexPaprikaSource::new("https://streaming.dexpaprika.com"),
        DEFAULT_MAX_AGE_SECS,
        DEFAULT_MAX_MOVE_BPS,
    );

    println!(
        "waiting for quotes on {}/{}…",
        pair.chain_id, pair.token_address
    );
    let mut stream = source.stream(&pair).take(5);
    while let Some(item) = stream.next().await {
        match item {
            Ok(tick) => println!("{} USD  published_at={}", tick.price_usd, tick.published_at),
            Err(error) => {
                eprintln!("stream error: {error}");
                // Print the useful cause hidden by reqwest's top-level error.
                for cause in error.chain().skip(1) {
                    eprintln!("  caused by: {cause}");
                }
            }
        }
    }
    Ok(())
}
