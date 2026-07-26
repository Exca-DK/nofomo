# nofomo

A non-custodial trading daemon steered over MCP. An AI agent host sets standing price rules; the daemon watches prices and acts on them by itself. The agent has no tool that quotes, signs or broadcasts a swap, so a rule is the only thing it can commit. See [docs/SKILL.md](docs/SKILL.md) for the agent contract.

Execution runs on Uniswap through its Trading API, and on Cetus for Sui CLMM pools when `sui.enabled` is set. One loop drives both: a venue builds, the vault signs, the chain client broadcasts. See [ARCHITECTURE.md](ARCHITECTURE.md) for architecture details.

## Running

`tempo-agentic-daemon` is the binary that trades. Broadcasting is off unless `MAINNET_SWAP=1` is set, so `run` quotes, builds and signs without sending anything. On Sui such a run also asks the node to simulate the transaction it built, so a run that spends nothing still says whether the swap would have worked; the answer lands in the order's failure reason.

Signing keys live in the files `keys.evm` and `keys.sui` point at, one raw key per file, owner-readable only. `bootstrap` creates whatever is missing; `keystore import` asks for an existing key without echoing it.

```bash
tempo-agentic-daemon bootstrap                 # create accounts; initialize the state DB if absent
tempo-agentic-daemon keystore import --chain evm
tempo-agentic-daemon health                    # also prints the addresses it will sign with
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

`strategy add`, `level add`, and `level rm` write straight into SQLite when nothing holds it. When `run` holds the database lock they call the daemon's own MCP tools instead, so a trading daemon never has to stop to be reconfigured. The daemon validates whatever it is asked to store, so the checks are the same either way.

### OpenClaw

`openclaw` prints the `mcp.servers.nofomo` entry for the running daemon, and `--write` merges it into `~/.openclaw/openclaw.json` without touching anything else in that file:

```bash
tempo-agentic-daemon openclaw
tempo-agentic-daemon openclaw --write
```

The entry carries a `toolFilter` that exposes only authoring: `set_strategy` and `set_level`, plus `strategies`, `levels` and `status` so an agent can see what already exists and whether a rule it stores will trade for real. `delete_level` and `orders` stay out. Widen the list in `openclaw.rs` if an agent should do more than write rules.

The daemon publishes a fresh port and token every start, so run this again after restarting it. The written file holds a bearer token and is kept owner-readable.

`dashboard` requires a running daemon. It reconnects through the daemon manifest after connection or authentication failures and renders feed and level states as text (for example `live`, `stale`, `armed`, or `cooldown`). Press `q` or Ctrl-C to detach; trading continues in the daemon.

### Prices

A price belongs to the asset, not to the chain it sits on — arbitrage keeps them level. EVM tokens are quoted by their own address, while each entry in `sui.coins` names a `price_ref`: the same asset on a chain the feed does index. That is how a testnet coin no feed has ever heard of still gets a price. A coin used only as a strategy's quote leg can omit it, but then it cannot be the base token a strategy is watched on.

Before an order is created, the venue's quote is checked against the price that fired the level and refused if it sits further away than `max_quote_deviation_bps` (5% by default). This catches what slippage cannot: `minimum_amount_out` is derived from the venue's own quote, so a bad route or a manipulated pool produces a self-consistent quote the daemon would otherwise sign. The check values the counter leg in dollars, so it needs `usd_peg: true` on the quote token — mark your stablecoins. An unpegged pair is logged as unchecked rather than silently trusted.

### Sui

Strategies on Sui name coins by the symbols in `sui.coins` and need `--venue cetus`:

```bash
tempo-agentic-daemon strategy add --id hbtc-sui --venue cetus --chain sui \
    --base-token hBTC --quote-token SUI
tempo-agentic-daemon level add --id sell-100k --strategy-id hbtc-sui \
    --side sell --trigger-price-usd 100000 --amount 0.001 --slippage-bps 100
```

Cetus needs a pool with real liquidity behind the pair. `create-pool` opens one and seeds it, signing with the same vault key the daemon trades from:

```bash
create-pool --config infra/local/config.json \
    --coin-a hBTC --coin-b SUI \
    --amount-a 100000 --amount-b 1000000000 --price 100000 --dry-run
```

`--dry-run` resolves the coins, reads their decimals on chain and prices the transaction without sending it. The registry defaults to the one in `crates/cetus/src/constants.rs`, which is the one this venue looks pools up in — Cetus runs more than one deployment on testnet, and a pool opened against another registry is one the daemon will never find. `--pools` overrides it only if Cetus redeploys.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
make build
```

## Layout

`apps/` contains binaries. `crates/` contains library components. `docs/` contains MCP integration guides. `infra/` contains local development infrastructure.
