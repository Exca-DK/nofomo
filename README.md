# nofomo

A non-custodial trading daemon steered over MCP. An AI agent host sets standing price rules; the daemon watches prices and acts on them by itself. The agent has no tool that quotes, signs or broadcasts a swap, so a rule is the only thing it can commit. See [docs/SKILL.md](docs/SKILL.md) for the agent contract.

Execution runs on Uniswap through its Trading API, and on Cetus for Sui CLMM pools when `sui.enabled` is set. One loop drives both: a venue builds, the vault signs, the chain client broadcasts. See [ARCHITECTURE.md](ARCHITECTURE.md) for architecture details.

## Running

`tempo-agentic-daemon` is the binary that trades. Broadcasting is off unless `MAINNET_SWAP=1` is set, so `run` quotes, builds and signs without sending anything.

Signing keys live in the files `keys.evm` and `keys.sui` point at, one raw key per file, owner-readable only. `bootstrap` creates whatever is missing; `keystore import` asks for an existing key without echoing it.

```bash
tempo-agentic-daemon bootstrap                 # create local dev accounts
tempo-agentic-daemon keystore import --chain evm
tempo-agentic-daemon health                    # also prints the addresses it will sign with
tempo-agentic-daemon level add --chain base --token-in USDC --token-out WETH \
    --side buy --trigger-price-usd 3000 --amount 2 --slippage-bps 50
tempo-agentic-daemon run
tempo-agentic-daemon resolve-quarantine --order-id o-1
```

### Prices

A price belongs to the asset, not to the chain it sits on — arbitrage keeps them level. EVM tokens are quoted by their own address, while each entry in `sui.coins` names a `price_ref`: the same asset on a chain the feed does index. That is how a testnet coin no feed has ever heard of still gets a price. A coin used only as the other leg of a swap can omit it, but then it cannot be the side a rule watches.

### Sui

Rules on Sui name coins by the symbols in `sui.coins` and need `--venue cetus`:

```bash
tempo-agentic-daemon level add --venue cetus --chain sui \
    --token-in hBTC --token-out SUI --side sell \
    --trigger-price-usd 100000 --amount 0.001 --slippage-bps 100
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
make check
make build
```

`make check` rebuilds the database schema before running tests.

## Layout

`apps/` contains binaries. `crates/` contains library components. `docs/` contains MCP integration guides. `infra/` contains local development infrastructure.
