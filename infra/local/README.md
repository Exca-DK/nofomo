# Local development setup

Two grids with real funds on EVM mainnets: ETH/USDC on Base, and CASHCAT/USDG on
Robinhood Chain. Five buy levels below the current price and five above it, each
worth one dollar, spaced 0.5% apart.

Every command below is either a CLI subcommand or an MCP tool. There is
deliberately no script in the repo: authoring belongs to the agent through MCP,
and a committed wrapper would route around that contract. The shell below is
here to read prices and do arithmetic, not to author anything by itself.

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

## 3. The grid is priced when you create it

No price is written down here. A grid is centred on the market at the moment you
create it, so both blocks below read the base token's price first and place the
levels around whatever comes back. The daemon watches the **base token**, so
that is the one to price.

Paste these two helpers into the shell first:

```bash
# Spot price of one token in dollars. DexPaprika refuses a request without a User-Agent.
price() {
  curl -fsS -H 'User-Agent: nofomo' \
      "https://api.dexpaprika.com/networks/$1/tokens/$2" |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["summary"]["price_usd"])'
}

# One rung of the grid: prints "<buy trigger> <sell trigger> <sell amount>".
# A buy spends one dollar of the quote token, so its amount is just 1.
# A sell spends one dollar of the base token, which is 1 / its trigger price.
grid() {
  python3 -c 'import sys
price, step, i = float(sys.argv[1]), float(sys.argv[2]), int(sys.argv[3])
buy, sell = price * (1 - step * i), price * (1 + step * i)
print(f"{buy:.8f} {sell:.8f} {1 / sell:.9f}")' "$1" "$2" "$3"
}
```

## 4. Base — ETH/USDC

```bash
eth=$(price base 0x4200000000000000000000000000000000000006)
echo "WETH: $eth USD"

tempo-agentic-daemon strategy add --id base-eth --chain base \
    --base-token WETH --quote-token USDC

for i in 1 2 3 4 5; do
  read -r buy sell size <<<"$(grid "$eth" 0.005 "$i")"
  tempo-agentic-daemon level add --id "base-eth-buy-$i" --strategy-id base-eth \
      --side buy --trigger-price-usd "$buy" --amount 1 --slippage-bps 100
  tempo-agentic-daemon level add --id "base-eth-sell-$i" --strategy-id base-eth \
      --side sell --trigger-price-usd "$sell" --amount "$size" --slippage-bps 100
done
```

## 5. Robinhood — CASHCAT/USDG

Of the tokenized stocks in the config only NVDA has a pool on this chain; AAPL
and TSLA are priced but have no liquidity to quote against. CASHCAT was picked
instead for its movement: 620k USD of pool liquidity, ~16% daily range, and a
USDG pair, which is what lets the quote check run at all.

```bash
cat=$(price robinhood 0x020bFC650A365f8bB26819DEaAbF3e21291018b4)
echo "CASHCAT: $cat USD"

tempo-agentic-daemon strategy add --id rh-cashcat --chain robinhood \
    --base-token CASHCAT --quote-token USDG

for i in 1 2 3 4 5; do
  read -r buy sell size <<<"$(grid "$cat" 0.005 "$i")"
  tempo-agentic-daemon level add --id "rh-cashcat-buy-$i" --strategy-id rh-cashcat \
      --side buy --trigger-price-usd "$buy" --amount 1 --slippage-bps 100
  tempo-agentic-daemon level add --id "rh-cashcat-sell-$i" --strategy-id rh-cashcat \
      --side sell --trigger-price-usd "$sell" --amount "$size" --slippage-bps 100
done
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

## Recentring later

Both blocks are keyed by level id, and a write to an existing id replaces it, so
running a block again reprices the grid around the current market. Two limits:
the daemon must not be holding the database lock, and a level that already fired
stays spent — rewriting its trigger does not re-arm it.

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
