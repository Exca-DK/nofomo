# CLAUDE.md

This file provides strict guidance to Claude Code (claude.ai/code) when working with code in this repository.

---

## Think Before Coding

Before implementing:

* State assumptions explicitly. If uncertain, ask.
* If multiple interpretations exist, present them — do not pick silently.
* If a simpler approach exists, say so. Push back when warranted.
* Design for data ownership first. If you are fighting the borrow checker, evaluate the architecture before reaching for `clone`, `Rc`, or lifetimes.

## Simplicity First

* No features beyond what was explicitly asked.
* Avoid complex lifetime annotations (`'a`) unless absolutely necessary. Clone data if it significantly simplifies the architecture and is not in a hot performance loop.
* No hyper-generic abstractions or traits for single-use code. Prefer concrete types.
* If you write 200 lines and it could be 50, rewrite it.

## Surgical Changes

* Do not "improve" adjacent code, comments, or formatting.
* Match existing codebase style exactly.
* Remove only imports, variables, or functions that YOUR changes made unused.
* Ensure all `rustfmt` and `clippy` checks pass cleanly before considering a task complete.

## Goal-Driven Execution

Transform tasks into verifiable goals:

* **Fixing Bugs:** Write a failing `#[test]` that reproduces it, then make it pass.
* **Refactoring:** Ensure `cargo test` passes completely before and after changes.

---

## Code Standards & Idioms

### Comments & Documentation

* **No Top-Level Module Docs:** Do not write module-level documentation (`//!`) at all. Rely entirely on clean file naming and clear code structure to communicate module purpose.

```rust
// ❌ Incorrect
//! SQLite connection pool setup and row serialization helpers.

```

* **Public Items Only:** Write `///` doc comments for public APIs (`pub`) so they are understood without reading the implementation. Document the conditions that cause **errors** or **panics**.
* **Skip the Obvious:** Do not document self-documenting code. Simple methods like `add()`, `new()`, or `is_empty()` need no comments.
* **Explain Why, Not How:** The code shows *how* it works. Comments must only explain *why* a specific approach, performance invariant, or hidden assumption exists.

```rust
// ✅ Correct
// TODO: optimize this loop once payload sizes exceed 1MB

```

* **Document Hacks & Workarounds:** If you write non-idiomatic code to bypass an upstream bug or API quirk, explicitly name the issue or library version so others don't "clean it up" and break it.
* **No Code Archaeology:** Never leave commented-out blocks of code or write comments tracking past bug fixes. Git handles history; keep the live file clean.
* **Keep It Simple & Lazy:** Use common words, short sentences, and everyday syntax. Avoid artificial formatting like numbered steps (`1. 2. 3.`), bullet lists, semicolons, or horizontal dividers (`---`). Make sure the comments and docs are short and simple. Ideally 1-2 sentences long.
* Comments and docs must explain enough so that it can be understood by anybody, even at first encounter or after a long break.

### Error Handling

* **Libraries / Domain Modules:** Use `thiserror` to define precise, typed domain errors. A library should never force `anyhow` on its callers.
* **Applications / Binaries:** Use `anyhow` for ergonomic propagation up to the application root. Map domain errors to a clear external representation (HTTP status, gRPC code, or CLI exit number) at the boundary — never leak internal error types to users.
* **Propagation:** Prefer the `?` operator over manual `match` on `Result`. Add context with `anyhow::Context::context` / `with_context` rather than discarding the cause.
* **Safety:** Never use `unwrap()` or `expect()` in production code unless accompanied by a comment explaining why failure is mathematically or logically impossible. `expect("reason")` is preferred over `unwrap()` because the message documents the invariant.

### Logging & Telemetry

* Use the `tracing` crate exclusively — never the standard `log` crate or `println!`/`eprintln!`.
* Leverage structured spans via `#[tracing::instrument]` on major functional boundaries to automatically capture context. Prefer structured fields (`tracing::info!(user_id, count, "…")`) over interpolating values into the message string.
* Install a subscriber only at the application entry point (binaries), typically `tracing-subscriber` with an `EnvFilter` driven by `RUST_LOG`. **Libraries must emit `tracing` events but never install a global subscriber** — that is the binary's choice.
* All log messages must start lowercase to maintain consistency across telemetry pipelines:
```rust
// ✅ Correct
tracing::info!(user_id, "starting data ingestion routine");

// ❌ Incorrect
tracing::info!(user_id, "Starting data ingestion routine");
```

### Async Rust

Applies only when the crate is async (uses `tokio` / an async runtime). Sync and pure-library crates can ignore this section.

* Do not perform heavy CPU-bound or blocking operations inside async functions. Offload them explicitly to `tokio::task::spawn_blocking`.
* **Never hold a `std::sync::Mutex`/`RwLock` guard across an `.await` point** — it can deadlock the runtime. Either drop the guard before awaiting, or use `tokio::sync` primitives when a lock genuinely must span an await.
* Prefer **bounded** channels (e.g. `tokio::sync::mpsc::channel(n)`) over unbounded ones so backpressure is explicit rather than unbounded memory growth.
* Prefer `impl Trait` or concrete generics over `Box<dyn Trait>` (dynamic dispatch) to keep execution fast and clear, unless dynamic dispatch is strictly required for heterogeneous collections or compilation speed.

### Project Structure

* Keep modules small and single-responsibility. Control visibility deliberately with `pub` / `pub(crate)`; default to private and widen only when needed.
* For anything beyond a trivial binary, put the logic in a library (`src/lib.rs` + modules) and keep `src/main.rs` a thin wrapper that parses config, sets up tracing, and starts the program. This keeps the core unit-testable and reusable. (Recommendation, not a mandate.)

### Testing & Verification

* **Unit Tests:** Place them at the bottom of the same file they test inside a localized `#[cfg(test)] mod tests { ... }` block.
* **Integration Tests:** Place them in the dedicated top-level `tests/` directory; these exercise the crate's public API only.
* **Mocking:** Prefer explicit traits and minimal hand-written test doubles. If complex mocking is necessary, use the `mockall` crate. Do not mock standard data structures.

### Documentation

* Write `///` doc comments on public items explaining purpose. Code examples in doc comments run as doctests under `cargo test`, so keep them compiling. Never use `//!` for module documentation.
* Documentation matters most for library crates whose API others consume. Verify docs build with `cargo doc --no-deps`.

### Dependency Hygiene

* Keep dependencies minimal and justified. Prefer the standard library or a single well-maintained crate over many overlapping ones.
* Avoid enabling default features you don't need; gate optional functionality behind your own feature flags.
* Pin the toolchain with a `rust-toolchain.toml` and declare a minimum supported Rust version (MSRV) so builds are reproducible. *(Recommended — not yet present in this repo.)*
* Centralize lints in `Cargo.toml` rather than scattering `#![allow(...)]` across files:
```toml
# Cargo.toml — recommended starting point
[lints.rust]
unsafe_code = "forbid"   # relax to "warn" only if unsafe is genuinely required

[lints.clippy]
all = { level = "warn", priority = -1 }
```

---

## Standard Commands

Always utilize standard cargo tooling to verify changes.

| Command | Description |
| --- | --- |
| `cargo check` | Fast compilation check to verify types and syntax. |
| `cargo fmt --all -- --check` | Verify that code complies with standard formatting. |
| `cargo clippy --all-targets --all-features -- -D warnings` | Run linter and treat all warnings as errors. |
| `cargo test` | Run the full test suite (unit + integration + doctests). |
| `cargo nextest run` | Faster, parallel test runner (optional; install via `cargo install cargo-nextest`). Does not run doctests — keep `cargo test --doc` for those. |
| `cargo audit` | Audit dependencies for crates with known security vulnerabilities (RUSTSEC). |
| `cargo deny check` | Stronger supply-chain gate: advisories + license policy + banned/duplicate crates. |
| `cargo machete` | Detect unused dependencies declared in `Cargo.toml`. |
| `cargo doc --no-deps` | Build this crate's API documentation. |

**Run a single test:**

```bash
cargo test test_name -- --exact
```

**Run tests with logging output visible:**

```bash
RUST_LOG=debug cargo test test_name -- --nocapture
```

---

## Continuous Integration

CI should gate every pull request with the following steps, in order, failing the build on any one:

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test` (or `cargo nextest run` plus `cargo test --doc`)

Cache the cargo registry and `target/` between runs for speed, and pin the toolchain via `rust-toolchain.toml` so CI and local builds match.

> No CI workflow files exist in this repository yet. Ask before adding them (e.g. a GitHub Actions workflow) if CI is wanted.
