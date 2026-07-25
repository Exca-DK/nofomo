ENV_LOCAL     := infra/local/.env
LOCAL_CONFIG  := infra/local/config.json
DEV_STATE_DIR := /tmp/tempo-agentic-dev

define load_env
	set -a && . $(ENV_LOCAL) && set +a &&
endef

.PHONY: check build bootstrap run health docker-build docker-run help

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
