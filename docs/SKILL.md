---
name: tempo-agentic
description: Tempo Agentic integration guide for setting the standing rules a trading daemon acts on.
version: 2.0.0
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
| `levels`       | none               | `LevelList`    | Every standing rule. |
| `orders`       | `limit` (optional) | `OrderList`    | Recent execution attempts, newest first. A read-only record of what already happened. |
| `set_level`    | `LevelDraft`       | `LevelView`    | Stores a rule, replacing one with the same id. **Spends funds once its price is crossed.** |
| `delete_level` | `id`               | `Deleted`      | Stops a rule firing again. Orders it already started are kept. |

## Rules

Check `status` before telling anyone a rule will trade for real: with `allow_broadcast` false, nothing reaches a chain.

Show a rule in full and get an explicit yes before calling `set_level`. It is the one tool that commits money, even though the spending happens later and without the agent.

Orders cannot be edited from here. A quarantined order is an operator's job: `tempo-agentic-daemon resolve-quarantine --order-id <id>`.
