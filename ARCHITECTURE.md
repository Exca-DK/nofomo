# Architecture

nofomo is a non-custodial trading daemon. A user-side daemon exposes MCP tools (`status`, `levels`, `orders`, `set_level`, `delete_level`) to an AI agent host. The agent only writes standing price levels; deciding when to act on one, and doing it, belongs to the daemon alone.

## Two planes

The user plane runs as one binary (`tempo-agentic-daemon`) on the user's machine, holds keys and local SQLite state, and makes all trade decisions. The system is non-custodial because execution depends on local key material.

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

Venues may read chain state directly through an RPC client. Only the chain client broadcasts, and only the signer signs. No venue ever reads a key file.

`TxContext`, `UnsignedTx`, and `SignedTx` are family-tagged enums, so one execution loop drives every venue. The transaction's variant picks the key, and a venue must build the family its `TxContext` carries. `crates/vault` holds one key per `ChainFamily` and is the only place key material lives; everything downstream sees `Arc<dyn Signer>`.

A plan names its own chain through `ExecutionPlan::chain`, and `ExecDeps` keys its node clients by that `ChainId`, so routing never guesses. `ChainClient` stays family-neutral; reads only EVM venues need (`balance_of`, `allowance`, `estimate_gas`) live on `EvmNode` instead, so no other family is asked questions its chain has no notion of.

An order persists one signed-transaction string. EVM stores raw EIP-2718 bytes; Sui keeps its signature detached from the transaction, so it stores both together and rebuilds them on the way back. Either way a restart rebroadcasts the identical transaction under the digest it already recorded.

## Execution and order state

One path runs plans to completion: `crates/orchestrator` advances standing levels through a state machine that persists after every transition, so a crash resumes rather than repeats. There is deliberately no second, synchronous path an agent could call — a swap has exactly one way to happen.

Execution-path code is kept small deliberately. Any growth should be a reviewed architectural decision.

Non-goals include multi-chain route selection, spend reservation systems, preflight simulation steps, and high-level strategy entities.

## Storage

SQLite is the daemon's only database. The cloud plane's `fills` table lives in Postgres in its own crate to maintain clean sqlx feature isolation.

## Messaging

NATS JetStream uses one subject space per chain so consumers remain isolated. Delivery is at-least-once with idempotency enforced by consumers.

## Status

This document describes the target shape. Migration occurs incrementally.
