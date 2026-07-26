# Local development setup

Two grids with real funds on EVM mainnets: ETH/USDC on Base, and CASHCAT/USDG on
Robinhood Chain. Five buy levels below the current price and five above it, each
worth one dollar, spaced 0.5% apart.

Every command below is either a CLI subcommand or an MCP tool. There is
deliberately no script: authoring belongs to the agent through MCP, and a shell
wrapper would route around that contract.

## 1. Secrets

```bash
cp infra/local/.env.example infra/local/.env
# fill in UNISWAP_API_KEY and GRAPH_API_KEY
```

## 2. Accounts

`bootstrap` creates a throwaway key. To trade with funds you already hold,
import yours instead — it is read from a masked prompt, never from the argv:

```bash
tempo-agentic-daemon bootstrap
tempo-agentic-daemon keystore import --chain evm
tempo-agentic-daemon health          # prints the address it will sign with
```

Fund that address before continuing. Each grid needs roughly five dollars of
each leg, plus gas:

| chain | needs |
|---|---|
| Base | 5 USDC and ~0.0026 WETH |
| Robinhood | 5 USDG and ~101 CASHCAT |

## 3. Levels are priced off the live market

A grid is centred on the price at the moment you create it, so read the price
first and put the levels around it. The daemon watches the **base token**, so
that is the one to price:

```bash
curl -s -H 'User-Agent: nofomo' \
  https://api.dexpaprika.com/networks/base/tokens/0x4200000000000000000000000000000000000006 \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["summary"]["price_usd"])'
```

For each level `i` in 1..5, at a step of 0.5%:

```
buy  trigger = price * (1 - 0.005 * i)     amount = 1          (spends the quote token, a dollar each)
sell trigger = price * (1 + 0.005 * i)     amount = 1 / trigger (spends one dollar of the base token)
```

## 4. Base — ETH/USDC

Values below assume a WETH price of **1885.53 USD**. Substitute the price you
just read; a grid built on a stale price is centred in the wrong place.

```bash
tempo-agentic-daemon strategy add --id base-eth --chain base \
    --base-token WETH --quote-token USDC

tempo-agentic-daemon level add --id base-eth-buy-1 --strategy-id base-eth --side buy \
    --trigger-price-usd 1876.100 --amount 1 --slippage-bps 100
tempo-agentic-daemon level add --id base-eth-buy-2 --strategy-id base-eth --side buy \
    --trigger-price-usd 1866.672 --amount 1 --slippage-bps 100
tempo-agentic-daemon level add --id base-eth-buy-3 --strategy-id base-eth --side buy \
    --trigger-price-usd 1857.245 --amount 1 --slippage-bps 100
tempo-agentic-daemon level add --id base-eth-buy-4 --strategy-id base-eth --side buy \
    --trigger-price-usd 1847.817 --amount 1 --slippage-bps 100
tempo-agentic-daemon level add --id base-eth-buy-5 --strategy-id base-eth --side buy \
    --trigger-price-usd 1838.390 --amount 1 --slippage-bps 100

tempo-agentic-daemon level add --id base-eth-sell-1 --strategy-id base-eth --side sell \
    --trigger-price-usd 1894.955 --amount 0.000527717 --slippage-bps 100
tempo-agentic-daemon level add --id base-eth-sell-2 --strategy-id base-eth --side sell \
    --trigger-price-usd 1904.383 --amount 0.000525105 --slippage-bps 100
tempo-agentic-daemon level add --id base-eth-sell-3 --strategy-id base-eth --side sell \
    --trigger-price-usd 1913.811 --amount 0.000522518 --slippage-bps 100
tempo-agentic-daemon level add --id base-eth-sell-4 --strategy-id base-eth --side sell \
    --trigger-price-usd 1923.238 --amount 0.000519956 --slippage-bps 100
tempo-agentic-daemon level add --id base-eth-sell-5 --strategy-id base-eth --side sell \
    --trigger-price-usd 1932.666 --amount 0.000517420 --slippage-bps 100
```

## 5. Robinhood — CASHCAT/USDG

Of the tokenized stocks in the config only NVDA has a pool on this chain; AAPL
and TSLA are priced but have no liquidity to quote against. CASHCAT was picked
instead for its movement: 620k USD of pool liquidity, ~16% daily range, and a
USDG pair, which is what lets the quote check run at all.

Read its price the same way, from
`https://api.dexpaprika.com/networks/robinhood/tokens/0x020bFC650A365f8bB26819DEaAbF3e21291018b4`.
Values below assume **0.048786 USD**.

```bash
tempo-agentic-daemon strategy add --id rh-cashcat --chain robinhood \
    --base-token CASHCAT --quote-token USDG

tempo-agentic-daemon level add --id rh-cashcat-buy-1 --strategy-id rh-cashcat --side buy \
    --trigger-price-usd 0.048542 --amount 1 --slippage-bps 100
tempo-agentic-daemon level add --id rh-cashcat-buy-2 --strategy-id rh-cashcat --side buy \
    --trigger-price-usd 0.048298 --amount 1 --slippage-bps 100
tempo-agentic-daemon level add --id rh-cashcat-buy-3 --strategy-id rh-cashcat --side buy \
    --trigger-price-usd 0.048054 --amount 1 --slippage-bps 100
tempo-agentic-daemon level add --id rh-cashcat-buy-4 --strategy-id rh-cashcat --side buy \
    --trigger-price-usd 0.047810 --amount 1 --slippage-bps 100
tempo-agentic-daemon level add --id rh-cashcat-buy-5 --strategy-id rh-cashcat --side buy \
    --trigger-price-usd 0.047566 --amount 1 --slippage-bps 100

tempo-agentic-daemon level add --id rh-cashcat-sell-1 --strategy-id rh-cashcat --side sell \
    --trigger-price-usd 0.049030 --amount 20.395834 --slippage-bps 100
tempo-agentic-daemon level add --id rh-cashcat-sell-2 --strategy-id rh-cashcat --side sell \
    --trigger-price-usd 0.049274 --amount 20.294864 --slippage-bps 100
tempo-agentic-daemon level add --id rh-cashcat-sell-3 --strategy-id rh-cashcat --side sell \
    --trigger-price-usd 0.049518 --amount 20.194890 --slippage-bps 100
tempo-agentic-daemon level add --id rh-cashcat-sell-4 --strategy-id rh-cashcat --side sell \
    --trigger-price-usd 0.049762 --amount 20.095895 --slippage-bps 100
tempo-agentic-daemon level add --id rh-cashcat-sell-5 --strategy-id rh-cashcat --side sell \
    --trigger-price-usd 0.050006 --amount 19.997866 --slippage-bps 100
```

## 6. Run

```bash
tempo-agentic-daemon level list          # confirm all 20 levels
tempo-agentic-daemon run                 # broadcasting OFF: quotes, builds, signs, sends nothing
tempo-agentic-daemon dashboard           # another terminal
```

With `MAINNET_SWAP` unset, an order reaches `Broadcasting` and then `Failed`
with "broadcast blocked" — that is the whole path proven without spending
anything. Only once that looks right:

```bash
MAINNET_SWAP=1 tempo-agentic-daemon run  # spends real funds
```

## Authoring through MCP instead

`strategy add`, `level add` and `level rm` write directly to the database and are
offline-only: while `run` holds the lock they refuse and point here. A running
daemon is authored through its MCP tools, which is the intended path for an
agent:

| tool | does |
|---|---|
| `set_strategy` | stores a market — same fields as `strategy add` |
| `set_level` | stores a level for an existing strategy |
| `delete_level` | removes one |
| `strategies`, `levels`, `orders`, `status` | read back what is stored and what ran |

The bearer token lives in the manifest next to the state database; `run` logs
its path on startup.

## What to expect from these grids

**A level fires once.** Any order that is not `Failed` spends its level for good,
so this is twenty single-shot orders, not a grid that keeps working.

**All five buys fire together.** A level fires when the price is *at or past* its
trigger, not when it crosses it. A drop to −2.5% therefore fires all five buy
levels in the same tick, spending five dollars at once rather than one dollar per
step on the way down.

**One dollar is a small trade.** Gas plus the venue fee is a few percent of a
one-dollar swap. That is fine for proving the path and poor economics otherwise.

**The quote is checked against the feed.** A quote further than
`max_quote_deviation_bps` (5%) from the price that fired the level is refused
before an order exists. If a level keeps refusing with "bps away from the
observed price", the venue and the feed disagree about that market — worth
understanding before raising the limit.
