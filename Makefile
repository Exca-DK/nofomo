ENV_LOCAL     := infra/local/.env
LOCAL_CONFIG  := infra/local/config.json
DEV_STATE_DIR := /tmp/tempo-agentic-dev
SQLX_DEV_DB   := /tmp/tempo-agentic-sqlx.db

define load_env
	set -a && . $(ENV_LOCAL) && set +a &&
endef

.PHONY: check build bootstrap run health prepare docker-build docker-run help

# Rebuilds the throwaway schema `sqlx::query!` type-checks against, then refreshes
# the committed .sqlx cache so builds work without a DATABASE_URL. Re-run after
# changing any SQL, or the next build fails on a stale cache.
# --all-targets matters: without it the cache misses queries used only in tests.
# Needs: cargo install sqlx-cli --no-default-features --features sqlite,rustls
prepare:
	rm -f $(SQLX_DEV_DB)
	sqlite3 $(SQLX_DEV_DB) < crates/storage/migrations/0001_audit.sql
	sqlite3 $(SQLX_DEV_DB) < crates/storage/migrations/0002_strategy.sql
	DATABASE_URL=sqlite://$(SQLX_DEV_DB) cargo sqlx prepare --workspace -- --all-targets

check:
	cargo fmt --all -- --check
	cargo test --workspace
	cargo clippy --workspace --all-targets -- -D warnings

build:
	cargo build --workspace --release

bootstrap: build
	$(load_env) ./target/release/tempo-agentic-admin bootstrap

run: build bootstrap
	$(load_env) ./target/release/tempo-agentic

health: build bootstrap
	$(load_env) ./target/release/tempo-agentic-admin health

help:
	@grep -E '^[a-zA-Z0-9_-]+:' Makefile \
	  | grep -v '^\.PHONY' \
	  | sed 's/:.*//' \
	  | sort \
	  | column -t
