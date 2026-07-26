# Architecture

nofomo is a non-custodial trading agent. A user-side daemon exposes MCP tools (`market_research`, `quote_trade`, `execute_trade`) to an AI agent host. It can optionally act on standing price levels via a cloud plane that only moves prices without holding funds or keys.

## Two planes

The user plane runs as one binary (`nofomo`) on the user's machine, holds keys and local SQLite state, and makes all trade decisions. The system is non-custodial because execution depends on local key material.

The cloud plane (`nofomo-relay`, `nofomo-indexer`, `nofomo-trigger`) provides stateless services that broadcast signed transactions, index fills, and stream prices. None of these services hold private keys or decide when to trade.

Apps communicate at runtime through NATS subjects and shared databases. Postgres is used for the cloud plane and SQLite for the daemon.

## Venue, chain, and signer split

A trade venue only produces quotes and builds unsigned transactions from accepted plans. It never communicates with a node or signs transactions.

```
TradeVenue::quote  -> ExecutionPlan
TradeVenue::steps  -> Vec<ExecStep>
TradeVenue::build  -> UnsignedTx
```

`ChainClient` handles node communication for execution, context, broadcasting, and confirmation. `Signer` turns an `UnsignedTx` into a `SignedTx`.

Venues may read chain state directly through an RPC client. Only the chain client broadcasts, and only the signer signs.

`ChainId` is family-tagged (`Evm(u64)` or `Sui`). `TxContext`, `UnsignedTx`, and `SignedTx` are family-tagged enums so one execution loop drives every venue.

## Execution and order state

Two paths run plans to completion. The synchronous path (`quote_trade` to `execute_trade`) drives plan steps in one call. The order-driven path in `crates/orchestrator` advances standing levels via a state machine that persists after transitions to handle crashes.

Execution-path code is kept small deliberately. Any growth should be a reviewed architectural decision.

Non-goals include multi-chain route selection, spend reservation systems, preflight simulation steps, and high-level strategy entities.

## Storage

SQLite is the daemon's only database. The cloud plane's `fills` table lives in Postgres in its own crate to maintain clean sqlx feature isolation.

## Messaging

NATS JetStream uses one subject space per chain so consumers remain isolated. Delivery is at-least-once with idempotency enforced by consumers.

## Status

This document describes the target shape. Migration occurs incrementally.
