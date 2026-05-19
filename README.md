<p align="center">
  <img src="./assets/dokono-rs-logo.png" alt="dokono-rs" width="100%">
</p>

# dokono-rs workspace

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](#license)

A family of static-analysis CLI tools for Rust workspaces that all answer the same shape of question — **"given this change, which X is affected?"** — by driving [rust-analyzer](https://rust-analyzer.github.io/) over LSP and walking a symbol-level reference graph.

None of these tools invoke `cargo build` or `--emit=dep-info`. Each one traces references through rust-analyzer instead, so even on dependency-heavy workspaces results usually come back in tens of seconds.

> The "dokono" (どこの) prefix means *"of which / from where"* in Japanese — every tool here answers a different "from where?" question.

## Crates in this workspace

| Crate | Kind | Status | What it answers |
|---|---|---|---|
| [`dokono-rs`](crates/dokono-rs/) | CLI (`dokono`) | [![Crates.io](https://img.shields.io/crates/v/dokono-rs.svg)](https://crates.io/crates/dokono-rs) | Which **binary entrypoints** are affected by a change? |
| [`dokono-test`](crates/dokono-test/) | CLI (`dokono-test`) | [![Crates.io](https://img.shields.io/crates/v/dokono-test.svg)](https://crates.io/crates/dokono-test) | Which **test functions** are affected by a change? |
| [`dokono-core`](crates/dokono-core/) | Internal library | [![Crates.io](https://img.shields.io/crates/v/dokono-core.svg)](https://crates.io/crates/dokono-core) | Shared LSP client, BFS engine, git-diff parsing, and `cargo metadata` plumbing used by the CLIs above. API is unstable. |

Each CLI has its own README with motivation, installation, full usage, and troubleshooting — start there if you want to use one of the tools.

## Shared architecture

All CLIs follow the same pipeline, implemented once in [`dokono-core`](crates/dokono-core/):

1. **Parse the git diff** (`gix`) into changed files + changed line numbers.
2. **Resolve starting symbols** via `textDocument/documentSymbol` — the innermost function/method enclosing each changed line.
3. **BFS upward** through `textDocument/references` (with `textDocument/declaration` to normalize `impl method → trait method` for trait-object dispatch), re-running `documentSymbol` at each reference site to step to the enclosing caller.
4. **Match visited positions** against a tool-specific *goal set*:
   - `dokono-rs` → bin entrypoint paths from `cargo metadata`
   - `dokono-test` → `#[test]` / `#[tokio::test]` / ... bodies discovered by `syn`

Readiness is detected deterministically by waiting for an `experimental/serverStatus` notification with `quiescent: true` — no fixed sleeps. The one bounded wait is a 30-second cap on the retry that follows a `-32801 ContentModified` response, so a single non-quiescing request cannot stall the whole BFS; the offending request is skipped and the traversal continues.

## Why this trade-off

Cargo's dependency graph resolves at the **crate level**. That is too coarse for any of the questions above: a one-line change in a foundational crate fans out across the workspace at the crate level even when the actual symbol is reached by only a handful of callers.

By going one level deeper — to the symbol-level reference graph that rust-analyzer already maintains — these tools answer the question that the crate-level graph cannot, without paying the cost of a full build.

The trade-off is that rust-analyzer has to index the workspace once on startup (tens of seconds with a warm cache, minutes on a cold cache for sizeable workspaces). This is intentional: correctness over raw latency.

## Requirements (shared)

| | |
|---|---|
| Rust toolchain | stable (required to build) |
| rust-analyzer | must be on `$PATH` (install with `rustup component add rust-analyzer`) |
| Git | any version |
| Target workspace | `cargo metadata` must succeed. The code does not need to compile, but dependencies must resolve so rust-analyzer can index. |

## License

[MIT License](LICENSE)
