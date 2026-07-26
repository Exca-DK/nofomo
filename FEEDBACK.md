# Uniswap Trading API feedback

From building fomono, a price-trigger trading bot for tokenized stocks on Robinhood Chain. We use the trading api for both execution and pricing.

## Good

Using `/v1/quote` as a price source works well

## Rough edges

- **Simulation vs forks.** `simulateTransaction` runs against real chain state, so anvil-funded
  accounts fail even when the tx is fine. Means a config flag we have to remember to flip.
- **Docs.** No canonical list of routes on the gateway - we found them by probing.
- **`priceImpact` units aren't documented.** It's a percentage.

Project: <https://github.com/mejordev/nofomo>