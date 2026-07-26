ENV_LOCAL     := infra/local/.env
LOCAL_CONFIG  := infra/local/config.json
DEV_STATE_DIR := /tmp/tempo-agentic-dev
SQLX_DEV_DB   := /tmp/tempo-agentic-sqlx.db

GRID_RUNGS        := 5
GRID_STEP_BPS     := 50
GRID_USD          := 1
GRID_SLIPPAGE_BPS := 100

define load_env
	set -a && . $(ENV_LOCAL) && set +a &&
endef

# Address of a token on one chain, read from the config the daemon itself loads
# so a grid can never be priced off a token this repo names somewhere else.
define token_address
$$(python3 -c 'import json, os, sys; chains = json.load(open(os.environ["TEMPO_AGENTIC_CONFIG"]))["evm"]["chains"]; print(next(c["tokens"][sys.argv[2]]["address"] for c in chains if c["name"] == sys.argv[1]))' $(1) $(2))
endef

# One grid: GRID_RUNGS levels below the live price and as many above, each worth
# GRID_USD. Level ids are derived from the strategy id, so running this again
# reprices that same grid rather than adding a second one. A level that already
# fired stays spent.
# Args: 1 strategy id, 2 chain (also the DexPaprika network), 3 base, 4 quote.
define grid_strategy
	$(load_env) ./target/release/tempo-agentic-daemon strategy add \
	    --id $(1) --chain $(2) --base-token $(3) --quote-token $(4)
	@$(load_env) price=$$(curl -fsS -H 'User-Agent: nofomo' \
	      https://api.dexpaprika.com/networks/$(2)/tokens/$(call token_address,$(2),$(3)) \
	    | python3 -c 'import json, sys; print(json.load(sys.stdin)["summary"]["price_usd"])') \
	  && echo "$(1): $(3) at $$price USD" \
	  && for i in $$(seq 1 $(GRID_RUNGS)); do \
	    set -- $$(python3 -c 'import sys; p, b, i, u = float(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]), float(sys.argv[4]); lo, hi = p * (1 - b * i / 1e4), p * (1 + b * i / 1e4); print(f"{lo:.8f} {hi:.8f} {u / hi:.9f}")' \
	        "$$price" $(GRID_STEP_BPS) $$i $(GRID_USD)) \
	    && buy=$$1 sell=$$2 size=$$3 \
	    && ./target/release/tempo-agentic-daemon level add --id $(1)-buy-$$i \
	        --strategy-id $(1) --side buy --trigger-price-usd $$buy \
	        --amount $(GRID_USD) --slippage-bps $(GRID_SLIPPAGE_BPS) \
	    && ./target/release/tempo-agentic-daemon level add --id $(1)-sell-$$i \
	        --strategy-id $(1) --side sell --trigger-price-usd $$sell \
	        --amount $$size --slippage-bps $(GRID_SLIPPAGE_BPS) \
	    || exit 1; \
	  done
endef

# Rebuild the sqlx validation database from the shipped schema.
export DATABASE_URL = sqlite://$(SQLX_DEV_DB)

.PHONY: schema check build bootstrap run health grid prepare require-env docker-build docker-run help

# Fail early when the uncommitted local environment file is missing.
require-env:
	@[ -f $(ENV_LOCAL) ] || { \
	  echo "missing $(ENV_LOCAL)" >&2; \
	  echo "run: cp $(ENV_LOCAL).example $(ENV_LOCAL), then fill in your API keys" >&2; \
	  exit 1; }

schema:
	rm -f $(SQLX_DEV_DB)
	sqlite3 $(SQLX_DEV_DB) < crates/storage/schema.sql

# Refresh the bootstrap cache for all targets; `check` still validates live.
# Keep output visible because failure may leave the cache empty.
# Needs: cargo install sqlx-cli --no-default-features --features sqlite,rustls
prepare: schema
	# Force macro recompilation because Cargo does not track .sqlx as an input.
	cargo clean -p tempo-agentic-storage
	cargo sqlx prepare --workspace -- --all-targets

check: schema
	cargo fmt --all -- --check
	cargo test --workspace
	cargo clippy --workspace --all-targets -- -D warnings

build: schema
	cargo build --workspace --release

bootstrap: require-env build
	$(load_env) ./target/release/tempo-agentic-daemon bootstrap

run: require-env build bootstrap
	$(load_env) ./target/release/tempo-agentic-daemon run

health: require-env build bootstrap
	$(load_env) ./target/release/tempo-agentic-daemon health

# Development shortcut for the two grids in infra/local/README.md. Writes to the
# database directly, so it only works while `run` is stopped; a running daemon is
# authored through its MCP tools instead.
grid: require-env build bootstrap
	$(call grid_strategy,base-eth,base,WETH,USDC)
	$(call grid_strategy,rh-cashcat,robinhood,CASHCAT,USDG)
	$(load_env) ./target/release/tempo-agentic-daemon level list

help:
	@grep -E '^[a-zA-Z0-9_-]+:' Makefile \
	  | grep -v '^\.PHONY' \
	  | sed 's/:.*//' \
	  | sort \
	  | column -t
