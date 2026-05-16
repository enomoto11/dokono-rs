# dokono-cve

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](#license)
[![Crates.io](https://img.shields.io/crates/v/dokono-cve.svg)](https://crates.io/crates/dokono-cve)

A CLI tool that decides whether a **vulnerable function in one of your dependencies** is actually reachable from any of your workspace's `bin` entrypoints — without running a build. It uses [rust-analyzer](https://rust-analyzer.github.io/) over LSP as a static-analysis backend, the same machinery that drives the sibling [`dokono-rs`](https://crates.io/crates/dokono-rs) CLI.

> The name follows the family convention: `dokono` (どこの) is Japanese for "of which / from where". `dokono-rs` answers "which binary is affected by this change?"; `dokono-cve` answers "from which binary is this CVE actually reachable?".

`dokono-cve` is published from the same Cargo workspace as `dokono-rs` and shares its analysis machinery via the internal [`dokono-core`](https://crates.io/crates/dokono-core) library.

Status: **early MVP** — see [issue #18](https://github.com/enomoto11/dokono-rs/issues/18) for the spec and roadmap.

---

Contents:

- [Motivation](#motivation)
- [How it works](#how-it-works)
- [Installation](#installation)
- [Usage](#usage)
  - [From a single advisory ID](#from-a-single-advisory-id)
  - [From a `cargo audit --json` dump](#from-a-cargo-audit---json-dump)
  - [From a single symbol path (0-days or DB-less use)](#from-a-single-symbol-path-0-days-or-db-less-use)
  - [Output](#output)
  - [Exit codes](#exit-codes)
  - [Diagnostic flags](#diagnostic-flags)
- [Side-effect policy](#side-effect-policy)
- [Requirements](#requirements)
- [Known limitations](#known-limitations)
- [Troubleshooting](#troubleshooting)
- [License](#license)

## Motivation

`cargo audit` answers a coarse question: *does my workspace depend on a crate that has a known vulnerability?* It is a great trip-wire, but it produces a flat list of advisories at the **crate** level. In a real workspace this often means:

- The vulnerable function is **never called** by any of your code — only by other deps.
- The function **is** called, but only from one of several binaries (the migrator, not the public API).
- The function is called only from `#[cfg(test)]` code that never ships.

Without finer information, the only options are "patch every advisory" or "ignore every advisory". `dokono-cve` adds three filters on top of `cargo audit`:

1. **Symbol level**: of the advisories your DB has narrowed down to specific functions (RUSTSEC `affected.functions`), does any code in your workspace actually call that *function*?
2. **Bin level**: from which of your `bin` entrypoints can it be reached?
3. **Production vs tests**: paths reachable only from `#[cfg(test)]` code are separated from production paths, so CI can fail only on real reachability.

Concretely, on a workspace with two bins:

```text
$ cargo audit
Crate:     openssl
Version:   0.10.68
Vulnerable: RUSTSEC-2025-0022 ...
```

becomes:

```text
$ dokono-cve --workspace . --audit-json <(cargo audit --json)
RUSTSEC-2025-0022  openssl  openssl::md::Md::fetch
  REACHABLE
    services/src/bin/api.rs       # patch now
  NOT REACHABLE
    services/src/bin/migrate.rs   # safe; defer
```

## How it works

```text
input (cargo audit / advisory id / symbol path)
       │
       ▼
parse to (crate, function path, version-req) tuples
       │
       ▼  for each (crate, version):
       │   - read user's `Cargo.lock` to confirm dep + version
       │   - synthesize a tiny standalone crate at
       │       ~/.cache/dokono-cve/probe-<crate>-<version>/
       │   - spawn a short-lived rust-analyzer there
       │   - textDocument/definition the path string
       ▼
seed = (file, position) inside ~/.cargo/registry/...
       │
       ▼  spawn a second rust-analyzer at the user's workspace
       │   and run dokono-core's upward BFS twice from the seed:
       │     1. unfiltered          → all reachable bins
       │     2. drop refs gated by  → production-only reachable bins
       │        `#[cfg(test)]`
       ▼
per-bin classification:
  production   = reachable in run #2
  tests_only   = reachable in run #1 but not run #2
  not_reachable = reachable in neither
```

Key points:

- Seed resolution is delegated to rust-analyzer's name resolver via a synthesized use-site. This handles `pub use` re-exports and alias paths correctly without re-implementing Rust's name resolution.
- The probe never touches the user's workspace — see [Side-effect policy](#side-effect-policy).
- The two BFS passes share the same rust-analyzer process, so the indexing cost is paid once per workspace.

## Installation

From crates.io:

```bash
cargo install dokono-cve
```

Or build from source:

```bash
git clone https://github.com/enomoto11/dokono-rs
cd dokono-rs
cargo build --release -p dokono-cve
# binary is produced at ./target/release/dokono-cve
```

`rust-analyzer` must be on `$PATH`:

```bash
rustup component add rust-analyzer
```

## Usage

Pick one of three input modes. `--workspace <PATH>` is required in all of them.

### From a single advisory ID

```bash
dokono-cve --workspace /path/to/workspace --advisory RUSTSEC-2025-0022
```

The advisory is looked up in the local [rustsec/advisory-db](https://github.com/rustsec/advisory-db) checkout (`~/.cargo/advisory-db`). Make sure `cargo audit fetch` or a recent `cargo audit` run has populated it.

### From a `cargo audit --json` dump

The recommended CI shape:

```bash
cargo audit --json > /tmp/audit.json
dokono-cve --workspace /path/to/workspace --audit-json /tmp/audit.json
```

Advisories whose RUSTSEC entry includes [`affected.functions`](https://github.com/rustsec/advisory-db/blob/main/EXAMPLE_ADVISORY.md) (Tier A) are analyzed; the rest are reported separately as "Unsupported advisories" — those degrade gracefully to the same precision as `cargo audit`.

### From a single symbol path (0-days or DB-less use)

```bash
dokono-cve --workspace /path/to/workspace \
           --symbol openssl::md::Md::fetch
```

Useful when:

- A vulnerability has been disclosed but not yet published to the advisory DB.
- A teammate flagged a specific function as risky and you want to find which of your bins call it.

### Output

Two output formats, selected with `--format` (default: `text`):

#### `--format text` (human-friendly)

```text
Workspace: /Users/you/your-workspace (3 bins)

RUSTSEC-2025-0022  openssl  openssl::md::Md::fetch
  seed: /Users/you/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/openssl-0.10.68/src/md.rs:99:12
  REACHABLE
    services/src/bin/api.rs
  REACHABLE FROM TESTS ONLY
    (none)
  NOT REACHABLE
    services/src/bin/migrate.rs
    services/src/bin/worker.rs
```

Bin paths are workspace-relative. Sections are emitted in a fixed order, deterministic across runs.

Progress lines (rust-analyzer startup, indexing, BFS) go to **stderr**; only the final result goes to **stdout**. This means `--format json` can always be piped safely.

#### `--format json` (CI / scripting)

```bash
dokono-cve --workspace . --audit-json /tmp/audit.json --format json | jq
```

```json
{
  "schema_version": 1,
  "workspace": "/Users/you/your-workspace",
  "all_bins": [
    "services/src/bin/api.rs",
    "services/src/bin/migrate.rs",
    "services/src/bin/worker.rs"
  ],
  "advisories": [
    {
      "advisory_id": "RUSTSEC-2025-0022",
      "crate": "openssl",
      "vulnerable_symbol": "openssl::md::Md::fetch",
      "seed_file": "/Users/you/.cargo/registry/.../openssl-0.10.68/src/md.rs",
      "seed_line": 99,
      "seed_character": 12,
      "bins": [
        { "path": "services/src/bin/api.rs",     "reachability": "production" },
        { "path": "services/src/bin/migrate.rs", "reachability": "not_reachable" },
        { "path": "services/src/bin/worker.rs",  "reachability": "not_reachable" }
      ]
    }
  ],
  "unresolved": [],
  "unsupported": []
}
```

| Field | Type | Notes |
|---|---|---|
| `schema_version` | int | Currently `1`. Bumped on incompatible schema changes. |
| `workspace` | string | The canonical workspace path. |
| `all_bins` | string[] | Workspace-relative bin paths, sorted. |
| `advisories[]` | object[] | One entry per Tier A symbol that was successfully resolved + BFS'd. |
| `advisories[].advisory_id` | string \| null | `null` for `--symbol` input. |
| `advisories[].bins[].reachability` | enum | `production` \| `tests_only` \| `not_reachable`. |
| `unresolved[]` | object[] | Tier A symbols where the dep is absent / out of range / probe failed. |
| `unsupported[]` | object[] | Advisories without `affected.functions` in the DB. |

### Exit codes

Designed so a single `dokono-cve` invocation can gate CI:

| Code | Meaning |
| ---- | ------- |
| `0`  | No production-reachable vulnerability found. |
| `1`  | Production-reachable vulnerability found. With `--strict`, also returned when only tests-only paths are reachable. |
| `2`  | Tool error (parse error, missing workspace, rust-analyzer crash, etc.). |

Example CI gate:

```bash
cargo audit --json > audit.json
dokono-cve --workspace . --audit-json audit.json
# exit 0 → green; exit 1 → fail; exit 2 → investigate the tool
```

### Diagnostic flags

```bash
dokono-cve ... --dry-run       # parse input only, then exit. Useful for confirming
                               # which advisories were classified Tier A vs Tier B.
dokono-cve ... --resolve-only  # parse + probe (resolve seeds), then exit.
                               # Useful for verifying the registry path before
                               # paying the cost of the full BFS.
dokono-cve ... --strict        # treat tests-only reachable bins as exit 1.
```

Logging is powered by [`tracing`](https://crates.io/crates/tracing) and controlled via the standard `RUST_LOG` env var (default: `warn`):

```bash
RUST_LOG=dokono_cve=info dokono-cve --workspace . --symbol ...
```

## Side-effect policy

`dokono-cve` does not touch the user's workspace at any point:

| Location | What happens |
|---|---|
| User workspace | **Untouched.** No files written, no `Cargo.toml` modified, no `cargo` commands that mutate state. |
| `~/.cache/dokono-cve/probe-<crate>-<version>/` | Probe crates (each ~3 lines of Rust) are written here once per `(crate, version)` and reused on subsequent runs. Safe to delete. |
| `~/.cargo/registry/` | Already populated by `cargo`. First-time probe of a new dep may trigger a download via `cargo metadata`. |

The reason for the external probe — instead of writing a use-site inside the user's workspace — is that `dokono-cve` must be safe to run in any working tree, including dirty branches and CI checkouts, without producing spurious diffs.

## Requirements

| | |
|---|---|
| Rust toolchain | stable (required to build the CLI) |
| rust-analyzer | must be on `$PATH` (install with `rustup component add rust-analyzer`) |
| Cargo | a valid workspace `Cargo.toml`; the code does not need to compile, but `cargo metadata` must succeed. |
| `cargo-audit` (optional) | only required if you intend to feed `--audit-json` from a `cargo audit` run; the binary is otherwise not needed. |

## Known limitations

- **`affected.functions` coverage.** Many RUSTSEC advisories do not declare specific vulnerable functions. `dokono-cve` surfaces these under "Unsupported advisories"; for those the precision is no better than `cargo audit`'s crate-level alert. Coverage is highest on `openssl`-class crates and lower elsewhere.
- **No intermediate-crate tracing.** A chain like *your_bin → third-party crate → vulnerable_dep* is not followed: rust-analyzer's reference search is scoped to the local workspace, and code under `~/.cargo/registry` is not traversed. The tool's contract is "reachable via direct workspace calls", not "reachable through anything in your dep tree".
- **`#[cfg(test)]` detection is shallow.** Matches `#[cfg(test)]` and `#[cfg(any(test, ...))]` on inline items (`fn`, `mod`, `impl`, etc.). Cross-file mods (`#[cfg(test)] mod foo;` paired with `foo.rs`) are not yet followed.
- **Vulnerable-crate source must be on crates.io.** The probe pins the dep with `=X.Y.Z` and relies on `cargo` resolving it from the registry. Git deps, path deps, and private registries for the vulnerable crate are not supported.
- **Macro-heavy code may underreport.** As in `dokono-rs`, heavy proc-macro / declarative-macro usage can leave rust-analyzer's reference resolution incomplete in some spots.
- **No call-chain rendering.** The MVP reports per-bin reachability but does not yet show the `seed → caller → ... → main` chain. Tracked as a follow-up.
- **Single rust-analyzer per workspace.** The full BFS spawns a second rust-analyzer at the user's workspace and waits for indexing; on a sizeable workspace expect tens of seconds to a few minutes for the first run (subsequent runs hit cargo's warm cache).

## Troubleshooting

### `no_dependency` for a crate I clearly depend on

The probe uses `cargo metadata` on the user's workspace and looks for a package whose name exactly matches the advisory crate. If the dep is renamed via `package = ...` in `Cargo.toml`, or the advisory crate name differs from the published one (rare), the match fails. Pass `--symbol <crate>::<path>` directly to bypass the lookup.

### `probe_failed: no definition returned for ...`

Means rust-analyzer could not name-resolve the symbol path inside the probe crate. Common causes:

1. **Symbol does not exist in the resolved version.** Verify the path against the source at `~/.cargo/registry/src/.../<crate>-<version>/src/`.
2. **Symbol is behind a feature flag** that the probe does not enable. The probe pulls the dep with its default features only. Workaround: until this is configurable, file an issue or use `--symbol` after manually verifying the path is unconditionally available.
3. **rust-analyzer not on PATH.** Check with `which rust-analyzer`.

### Slow first run

The probe crate triggers a `cargo metadata` that may fetch the vulnerable crate's source on first use; then both rust-analyzer instances index. Subsequent runs reuse the cached probe crate and rust-analyzer's incremental cache. **Use `--release` builds of `dokono-cve` itself** if invocation latency matters.

### `error loading advisory database: ... unsupported CVSS version: 4.0`

This is a `rustsec` / `cargo-audit` issue, not a `dokono-cve` issue: the local `~/.cargo/advisory-db` contains newer CVSS 4.0 entries that the installed `rustsec` cannot parse. Update `cargo-audit` (or wait for a `rustsec` release with CVSS 4.0 support). Until then, prefer `--audit-json` (generated by a working cargo-audit elsewhere) or `--symbol` over `--advisory`.

## License

[MIT License](../../LICENSE)
