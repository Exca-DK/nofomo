---
name: tempo-agentic
description: Tempo Agentic integration guide for setting the standing rules a trading daemon acts on.
version: 3.0.0
---

# Tempo Agentic — Agent Integration Guide

This is the contract for an agent host talking to a running `tempo-agentic-daemon`.

**The agent cannot trade.** No tool quotes, signs or broadcasts a swap. The agent writes standing rules; the daemon watches prices and acts on them by itself, deterministically. That split is the security model, so nothing here should be worked around.

## Reaching the daemon

The daemon serves MCP over HTTP on loopback and writes its address and bearer token to `<state_db_path>.mcp.json`, readable by its owner alone. The file exists only while the daemon runs.

## MCP surface

| tool           | params             | returns        | notes |
| -------------- | ------------------ | -------------- | ----- |
| `status`       | none               | `DaemonStatus` | Read first. `allow_broadcast` false means rules are quoted, built and signed but never sent. |
| `strategies`   | none               | `StrategyList` | Every configured market. |
| `set_strategy` | `StrategyDraft`    | `StrategyView` | Stores a market. It can change only before its first level is added. |
| `levels`       | none               | `LevelList`    | Every standing rule, including its `strategy_id` and resolved direction. |
| `orders`       | `limit` (optional) | `OrderList`    | Recent execution attempts, newest first. A read-only record of what already happened. |
| `set_level`    | `LevelDraft`       | `LevelView`    | Stores a rule for an existing strategy, replacing one with the same id. **Spends funds once its price is crossed.** |
| `delete_level` | `id`               | `Deleted`      | Stops a rule firing again. Orders it already started are kept. |

## Rules

Check `status` before telling anyone a rule will trade for real: with `allow_broadcast` false, nothing reaches a chain.

Show strategy and level changes in full and get an explicit yes before calling `set_strategy` or `set_level`. A level commits money even though the spending happens later and without the agent.

`StrategyDraft` contains `id`, `venue`, `chain`, `base_token`, and `quote_token`. `LevelDraft` contains `id`, `strategy_id`, `side`, `trigger_price_usd`, `amount`, and `slippage_bps`. A `buy` spends the strategy's quote token for its base token (`quote -> base`); a `sell` spends base for quote (`base -> quote`). `amount` is in whole units of the token being spent.

While the daemon is running, direct CLI authoring refuses writes and points to MCP. Do not work around that lock with offline database access.

Orders cannot be edited from here. A quarantined order is an operator's job: `tempo-agentic-daemon resolve-quarantine --order-id <id>`.

The operator can attach `tempo-agentic-daemon dashboard` to a running daemon. It reconnects through the daemon manifest, shows textual feed and level statuses, and detaches with `q` or Ctrl-C without stopping trading. The state database uses a fresh-schema policy with no migrations; an old development database must be removed manually at the exact path reported by `bootstrap` or `run`.
