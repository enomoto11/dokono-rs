# dokono-cve

A CLI that decides whether a **vulnerable function in one of your dependencies** is actually reachable from any of your workspace's `bin` entrypoints. Built on the same rust-analyzer-driven upward BFS as [`dokono-rs`](../dokono-rs).

Status: **early MVP**. See [issue #18](https://github.com/enomoto11/dokono-rs/issues/18) for the spec.

## What it does

`cargo audit` answers "does my workspace depend on a crate with a vulnerability?" — a coarse, crate-level question. `dokono-cve` adds two finer filters on top:

1. **Symbol level**: does any code in my workspace actually call the *specific* vulnerable function?
2. **Bin level**: from which of my `bin` entrypoints can that function be reached?
3. **Production vs tests**: separate paths reachable only from `#[cfg(test)]` so CI can fail only on real reachability.

The intended workflow is: `cargo audit` finds the candidates, `dokono-cve` decides which ones are actually exploitable in your binaries.

## Usage

Pick one of three input modes plus a workspace path. Reachability is reported per bin: `production` / `tests_only` / `not_reachable`.

### From a single advisory

```bash
dokono-cve --workspace /path/to/your/workspace --advisory RUSTSEC-2025-0022
```

### From a `cargo audit --json` dump

```bash
cargo audit --json > /tmp/audit.json
dokono-cve --workspace /path/to/your/workspace --audit-json /tmp/audit.json
```

### From a single symbol path (for advisories without `affected.functions`, or 0-days)

```bash
dokono-cve --workspace /path/to/your/workspace --symbol openssl::md::Md::fetch
```

### Output formats

```bash
dokono-cve ... --format text    # default; human-readable sections
dokono-cve ... --format json    # for CI / piping to jq
```

Progress lines go to stderr; only the result goes to stdout, so `--format json` is always pipe-safe.

### Diagnostic flags

```bash
dokono-cve ... --dry-run         # parse input only; skip probe + BFS
dokono-cve ... --resolve-only    # resolve seed positions only; skip BFS
```

## Exit codes (intended for CI)

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

## How seed resolution works

`dokono-cve` does not touch your workspace. For each `(crate, version)` it needs to probe, it synthesizes a tiny standalone cargo crate under `~/.cache/dokono-cve/probe-<crate>-<version>/`, asks a short-lived rust-analyzer to `textDocument/definition` the path string, and reads the resulting `~/.cargo/registry/...` location. That registry path is shared with the user-side rust-analyzer that performs the actual BFS.

If the workspace does not depend on the crate, or the resolved version is outside the advisory's affected range, the symbol is short-circuited as `no_dependency` / `not_affected_version` — no probe is spawned.

## Known limitations

- **`affected.functions` coverage**: many RUSTSEC advisories do not declare the specific vulnerable functions. Such advisories surface in the output under "Unsupported advisories", with the same precision as plain `cargo audit`.
- **`#[cfg(test)]` detection is shallow**: matches `#[cfg(test)]` and `#[cfg(any(test, ...))]` on inline items. Cross-file mods (`#[cfg(test)] mod foo;` paired with `foo.rs`) are not followed.
- **No call-chain rendering** in this MVP. The output reports per-bin reachability but does not yet show the `seed → … → bin` chain. Tracked as a follow-up.
- **Indirect reachability through third-party crates is not traced.** rust-analyzer's reference scope covers the local workspace, not deps under `~/.cargo/registry`, so a chain like *your_bin → third_party → vulnerable_dep* is missed.
- **Git / path / private-registry deps for the vulnerable crate are not supported** by the probe (only crates.io is implemented for MVP).

## Requirements

- `rust-analyzer` on `$PATH`
- `cargo` + a valid workspace `Cargo.toml`

## License

[MIT License](../../LICENSE)
