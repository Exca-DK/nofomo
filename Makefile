ENV_LOCAL     := infra/local/.env
LOCAL_CONFIG  := infra/local/config.json
DEV_STATE_DIR := /tmp/tempo-agentic-dev
SQLX_DEV_DB   := /tmp/tempo-agentic-sqlx.db

define load_env
	set -a && . $(ENV_LOCAL) && set +a &&
endef

# Every `sqlx::query!` is validated against this database, which is rebuilt from
# the migrations on each run. Rebuilding rather than reusing is the point: a
# schema that can drift from the migrations would validate queries against
# something nobody ships.
export DATABASE_URL = sqlite://$(SQLX_DEV_DB)

.PHONY: schema check build bootstrap run health prepare require-env docker-build docker-run help

# $(ENV_LOCAL) is no longer committed, so a fresh clone has to be told what to
# copy. Listed first among the prerequisites so it fails in a second rather than
# after a full release build.
require-env:
	@[ -f $(ENV_LOCAL) ] || { \
	  echo "missing $(ENV_LOCAL)" >&2; \
	  echo "run: cp $(ENV_LOCAL).example $(ENV_LOCAL), then fill in your API keys" >&2; \
	  exit 1; }

schema:
	rm -f $(SQLX_DEV_DB)
	for f in crates/storage/migrations/*.sql; do sqlite3 $(SQLX_DEV_DB) < $$f; done

# Refreshes the committed .sqlx cache, which lets a fresh clone build before it
# has a database. The cache is a convenience, never the thing queries are checked
# against — `check` always validates live.
# --all-targets matters: without it the cache misses queries used only in tests.
# Never silence this target: `cargo sqlx prepare` clears .sqlx before repopulating
# it, so a failure here leaves an empty cache behind.
# Needs: cargo install sqlx-cli --no-default-features --features sqlite,rustls
prepare: schema
	# Force a rebuild of the crate holding the macros. `cargo sqlx prepare`
	# clears .sqlx and repopulates it from whatever compiles; cargo does not
	# treat .sqlx as an input, so an up-to-date build collects nothing and
	# leaves the cache empty.
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
