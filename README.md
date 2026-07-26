# nofomo

A non-custodial trading daemon steered over MCP. An AI agent host sets standing price rules; the daemon watches prices and acts on them by itself. The agent has no tool that quotes, signs or broadcasts a swap, so a rule is the only thing it can commit. See [docs/SKILL.md](docs/SKILL.md) for the agent contract.

Execution runs on Uniswap through its Trading API. `crates/cetus` holds local swap math for Sui CLMM pools but is not wired into the daemon yet. See [ARCHITECTURE.md](ARCHITECTURE.md) for architecture details.

## Running

`tempo-agentic-daemon` is the only binary. Broadcasting is off unless `MAINNET_SWAP=1` is set, so `run` quotes, builds and signs without sending anything.

```bash
tempo-agentic-daemon bootstrap                 # create local dev accounts
tempo-agentic-daemon import-key --chain evm --private-key 0x...
tempo-agentic-daemon health
tempo-agentic-daemon level add --chain base --token-in USDC --token-out WETH \
    --side buy --trigger-price-usd 3000 --amount 2 --slippage-bps 50
tempo-agentic-daemon run
tempo-agentic-daemon resolve-quarantine --order-id o-1
```

## Development

```bash
make check
make build
```

`make check` rebuilds the database schema before running tests.

## Layout

`apps/` contains binaries. `crates/` contains library components. `docs/` contains MCP integration guides. `infra/` contains local development infrastructure.
