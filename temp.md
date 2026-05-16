# Task: Extract `dokono-core` crate from dokono-rs

## Context

dokono-rs is a CLI tool that detects which Rust binary entrypoints
are affected by a git diff, by walking a symbol-level reference graph
via rust-analyzer over LSP. Today everything lives in a single crate.

I want to build sibling tools (dokono-cve-reach, dokono-test,
dokono-deadcode, dokono-blast-radius) that share the same core
machinery — LSP client, BFS over references, bin enumeration via
cargo metadata — but differ in:

  - what they use as BFS seeds
  - what they use as the goal set
  - what direction they traverse (caller-side vs callee-side)

So the goal of this task is to refactor the repo into a workspace
with a reusable `dokono-core` library crate and the existing
`dokono-rs` CLI as a thin consumer of it. No behavior change for
the CLI.

## Constraints

- The existing `dokono-rs` CLI must keep exactly the same:
  - command-line flags (`--workspace`, `--pr`, `--base`, `--head`,
    the `debug` subcommand and all its variants)
  - stdout / stderr output format
  - exit codes
- `cargo install dokono-rs` must continue to work and install the
  same binary name (`dokono-rs`).
- Public `dokono-core` API must support, at minimum:
  - "give me the enclosing function symbol at file:line"
  - "BFS upward from a set of starting positions, return reached
    symbols with parent pointers"
  - "BFS downward from a set of starting positions" (even if not
    used by dokono-rs today — needed for dokono-deadcode)
  - enumerate bin entrypoints from cargo metadata
  - the LSP client lifecycle: spawn rust-analyzer, wait for
    quiescent, shutdown
- BFS must expose parent pointers so callers can reconstruct call
  chains (needed for dokono-cve-reach's tree output).
- No new runtime dependencies unless strictly needed. Keep the
  rust-analyzer LSP client implementation in dokono-core.

## What I want you to do, in order

### Step 1: Read and report

Before writing any code, do the following and reply with your
findings:

1. Read the full source tree: `src/`, `tests/`, `Cargo.toml`,
   `.github/workflows/`, `.claude/`.
2. Identify the modules / functions that are CLI-specific (arg
   parsing, PR fetching via `git fetch`, stdout formatting, debug
   subcommand dispatch) vs core (LSP client, documentSymbol
   queries, references queries, BFS, cargo metadata parsing,
   diff parsing).
3. List the public API surface you propose for `dokono-core`:
   module structure, types, functions, with brief docstrings.
   No implementation yet, just signatures.
4. Call out anything ambiguous: code that could plausibly live
   on either side, or types that currently mix CLI concerns
   with core concerns (e.g. error types that include
   user-facing strings).

Output this as a markdown plan. **Stop and wait for my approval
before moving to Step 2.**

### Step 2: Refactor

Once I approve the plan:

1. Convert the repo to a Cargo workspace with two members:
   `crates/dokono-core` (library) and `crates/dokono-rs`
   (binary, depending on `dokono-core` via path).
2. Move code accordingly. Keep commits small and logically
   grouped (one commit per module moved, ideally).
3. Update `Cargo.toml` files. The CLI crate keeps the
   `dokono-rs` package name so `cargo install dokono-rs` still
   works.
4. Update any `use` paths in tests. Move tests that exercise
   core logic into `crates/dokono-core/tests/`; keep
   CLI-integration tests in `crates/dokono-rs/tests/`.
5. Update `.github/workflows/` if any paths are hardcoded.

### Step 3: Verify

Run, in this order, and show me the output of each:

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. `cargo build --release` and confirm the binary path
   `target/release/dokono-rs` still exists.
5. Run `./target/release/dokono-rs debug print-entrypoints
   --workspace .` against this repo itself, and confirm the
   output is identical to the same command run against the
   `main` branch before the refactor. (You can `git stash` and
   compare, or use a worktree.)

If any of these fail, stop and report. Do not paper over
failures.

### Step 4: Document

Add a short `crates/dokono-core/README.md` describing:

- What the crate is for
- Two minimal usage examples: one for "upward BFS from a
  symbol position" and one for "enumerate bin entrypoints"
- A note that the API is unstable pre-1.0

Update the root `README.md` only to mention the workspace
structure in one sentence near the top. Don't rewrite the
existing motivation/usage sections.

## Output rules

- Reply to Step 1 as a markdown plan and then stop.
- For Steps 2–4, show me each commit's diff summary (file list
  + brief description), not the full diffs.
- If you hit an ambiguity not covered by the constraints above,
  ask before guessing.
- Don't add features. Don't refactor things outside the
  extraction scope (e.g. don't "improve" the BFS algorithm
  while you're in there).