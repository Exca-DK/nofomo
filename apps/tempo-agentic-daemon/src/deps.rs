use std::sync::Arc;

use tempo_agentic_config::Config;
use tempo_agentic_price::{
    DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_MOVE_BPS, FilteredSource, PriceSource,
};
use tempo_agentic_price_dexpaprika::DexPaprikaSource;
use tempo_agentic_trigger::TokenResolver;

pub fn prices(config: &Config) -> Arc<dyn PriceSource> {
    Arc::new(FilteredSource::new(
        DexPaprikaSource::new(config.dexpaprika_stream_url.clone()),
        DEFAULT_MAX_AGE_SECS,
        DEFAULT_MAX_MOVE_BPS,
    ))
}

pub fn tokens(config: &Config) -> Arc<TokenResolver> {
    Arc::new(TokenResolver::from_config(&config.evm, &config.sui))
}
