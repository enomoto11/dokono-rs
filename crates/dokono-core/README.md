# dokono-core

Internal library that powers [`dokono-rs`](../dokono-rs) and its planned sibling tools (`dokono-cve-reach`, `dokono-deadcode`, `dokono-blast-radius`, ...).

It provides the shared machinery:

- a rust-analyzer LSP client (spawn, initialize, wait-quiescent, shutdown)
- batched `references` / `documentSymbol` / `declaration` requests
- an `LspBackend` trait and a wave-based BFS over the symbol-level reference graph
- `cargo metadata`-driven binary-entrypoint enumeration
- git diff parsing via `gix`

**API is unstable. There are no compatibility guarantees pre-1.0; minor releases may break any signature. If you want to detect impacted entrypoints from the command line, install `dokono-rs` instead.**

## Examples

### Upward BFS from a symbol position

```rust
use dokono_core::bfs;
use dokono_core::lsp::{backend::Backend, client::Client, lifecycle, progress};
use dokono_core::types::Position;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn main() -> anyhow::Result<()> {
    let workspace = Path::new(".").canonicalize()?;

    let mut client = Client::spawn(&workspace)?;
    lifecycle::initialize(&mut client, &workspace)?;
    progress::wait_for_index_end(&client)?;

    let mut backend = Backend::new(&client, workspace.clone());

    let file = workspace.join("src/lib.rs");
    backend.open(&file)?;

    let starts = vec![(file, Position { line: 9, character: 7 })];
    let goals: HashSet<PathBuf> = HashSet::new();

    let affected = bfs::run(&mut backend, starts, &goals)?;
    println!("affected entrypoints: {affected:?}");

    lifecycle::shutdown(&mut client)?;
    Ok(())
}
```

### Enumerate bin entrypoints

```rust
use dokono_core::entrypoints;
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let bins = entrypoints::load_bin_entrypoints(Path::new("."))?;
    for p in bins {
        println!("{}", p.display());
    }
    Ok(())
}
```
