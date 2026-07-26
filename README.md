# nofomo

A non-custodial trading daemon steered over MCP. An AI agent host sets standing price rules; the daemon watches prices and acts on them by itself. The agent has no tool that quotes, signs or broadcasts a swap, so a rule is the only thing it can commit. See [docs/SKILL.md](docs/SKILL.md) for the agent contract.

Execution runs on Uniswap through its Trading API. `crates/cetus` holds local swap math for Sui CLMM pools but is not wired into the daemon yet. See [ARCHITECTURE.md](ARCHITECTURE.md) for architecture details.

## Running

`tempo-agentic-daemon` is the only binary. Broadcasting is off unless `MAINNET_SWAP=1` is set, so `run` quotes, builds and signs without sending anything.

```bash
tempo-agentic-daemon bootstrap                 # create accounts; initialize the state DB if absent
tempo-agentic-daemon import-key --chain evm --private-key 0x...
tempo-agentic-daemon health
tempo-agentic-daemon strategy add --id eth-usdc --chain base \
    --base-token WETH --quote-token USDC
tempo-agentic-daemon strategy list
tempo-agentic-daemon level add --id buy-3000 --strategy-id eth-usdc \
    --side buy --trigger-price-usd 3000 --amount 2 --slippage-bps 50
tempo-agentic-daemon level list
tempo-agentic-daemon level rm --id buy-3000
tempo-agentic-daemon run
tempo-agentic-daemon dashboard                 # run in another terminal after `run`
tempo-agentic-daemon resolve-quarantine --order-id o-1
```

A strategy owns its market. Every level refers to one with `--strategy-id`: `buy` spends the strategy's quote token for its base token (`quote -> base`), while `sell` spends base for quote (`base -> quote`). `--amount` is expressed in whole units of the token being spent.

`bootstrap` and `run` initialize the current schema only when the state database does not exist. There is no schema migration: if an older development database is rejected, remove the exact path named in the error manually and run `bootstrap` or `run` again. The daemon never deletes it for you.

Direct `strategy add`, `level add`, and `level rm` are offline-only. If `run` holds the database lock, they refuse the write and direct authoring to the daemon's MCP tools; list commands remain available.

`dashboard` requires a running daemon. It reconnects through the daemon manifest after connection or authentication failures and renders feed and level states as text (for example `live`, `stale`, `armed`, or `cooldown`). Press `q` or Ctrl-C to detach; trading continues in the daemon.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
make build
```

## Layout

`apps/` contains binaries. `crates/` contains library components. `docs/` contains MCP integration guides. `infra/` contains local development infrastructure.
