# nofomo

A non-custodial trading agent exposed over MCP. An AI agent host talks to a local daemon over stdio or HTTP. The daemon exposes three tools: `market_research` for Graph pool data, `quote_trade` for venue quotes, and `execute_trade` to sign and broadcast swaps.

Supported venues include Uniswap via its Trading API, and Cetus with local swap math for Sui CLMM pools. See [ARCHITECTURE.md](ARCHITECTURE.md) for architecture details.

## Development

```bash
make check
make build
```

`make check` rebuilds the database schema before running tests.

## Layout

`apps/` contains binaries. `crates/` contains library components. `docs/` contains MCP integration guides. `infra/` contains local development infrastructure.
