---
name: tempo-agentic
description: Tempo Agentic Integration Guide for market research, quoting, and trade execution.
version: 1.0.0
---

# Tempo Agentic — Agent Integration Guide

This is the contract for OpenClaw when using the `tempo-agentic` MCP server. The server exposes tools for researching, quoting, and executing token swaps.

## MCP surface

The tools are available via stdio MCP.

| tool              | params                                     | returns                         | notes |
| ----------------- | ------------------------------------------ | ------------------------------- | ----- |
| `market_research` | `MarketResearchRequest` (base_token, quote_token) | `MarketResearch`                | Use to get pool data on The Graph before quoting |
| `quote_trade`     | `QuoteTradeRequest` (chains, amount, etc)  | `QuoteView`                     | Generates a quote. **Does not execute**. |
| `execute_trade`   | `ExecuteTradeRequest` (quote_id, confirmed)| `ExecutionView`                 | **Requires user confirmation**. Set `confirmed` to true. |

The tools should be invoked in order: `market_research` -> `quote_trade` -> `execute_trade`. Never invent quote IDs and never claim that funds are automatically bridged.
