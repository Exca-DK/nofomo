ENV_LOCAL     := infra/local/.env
LOCAL_CONFIG  := infra/local/config.json
DEV_STATE_DIR := /tmp/tempo-agentic-dev
SQLX_DEV_DB   := /tmp/tempo-agentic-sqlx.db

define load_env
	set -a && . $(ENV_LOCAL) && set +a &&
endef

# Rebuild the sqlx validation database from the shipped schema.
export DATABASE_URL = sqlite://$(SQLX_DEV_DB)

.PHONY: schema check build bootstrap run health prepare require-env docker-build docker-run help

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

help:
	@grep -E '^[a-zA-Z0-9_-]+:' Makefile \
	  | grep -v '^\.PHONY' \
	  | sed 's/:.*//' \
	  | sort \
	  | column -t
