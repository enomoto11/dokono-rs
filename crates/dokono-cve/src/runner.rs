//! Run upward BFS from registry-side seeds and classify each workspace bin as
//! Production / TestsOnly / NotReachable.
//!
//! Classification works by running the BFS twice over the same rust-analyzer session:
//! once with raw `references` results (all-reachable), and once with `#[cfg(test)]`-gated
//! reference locations filtered out (production-only). A bin reachable in both is
//! Production; a bin reachable only in the first run is TestsOnly. Two passes catch the
//! multi-path case where a bin has BOTH a test-only and a production path to the seed.

use anyhow::Result;
use dokono_core::bfs::{self, BfsDirection, LspBackend};
use dokono_core::entrypoints;
use dokono_core::lsp::backend::Backend;
use dokono_core::lsp::{client::Client, lifecycle, progress};
use dokono_core::types::{DocumentSymbol, Location, Position};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::cfg_test::CfgClassifier;
use crate::probe::ResolvedSeed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reachability {
    Production,
    TestsOnly,
    NotReachable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedReachability {
    pub seed: ResolvedSeed,
    pub bins: BTreeMap<PathBuf, Reachability>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    pub all_bins: BTreeSet<PathBuf>,
    pub per_seed: Vec<SeedReachability>,
}

pub fn run(workspace: &Path, seeds: Vec<ResolvedSeed>) -> Result<RunResult> {
    let bin_list = entrypoints::load_bin_entrypoints(workspace)?
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect::<Vec<_>>();
    let entrypoints: HashSet<PathBuf> = bin_list.iter().cloned().collect();
    let all_bins: BTreeSet<PathBuf> = bin_list.iter().cloned().collect();

    if entrypoints.is_empty() {
        anyhow::bail!("no binary entrypoints found in {}", workspace.display());
    }

    tracing::info!(
        "bfs: spawning rust-analyzer at {} ({} entrypoints, {} seeds)",
        workspace.display(),
        entrypoints.len(),
        seeds.len()
    );
    let mut client = Client::spawn(workspace)?;
    lifecycle::initialize(&mut client, workspace)?;
    progress::wait_for_index_end(&client)?;

    let mut backend = Backend::new(&client, workspace.to_path_buf());

    let mut per_seed = Vec::with_capacity(seeds.len());
    for seed in seeds {
        backend.open(&seed.file)?;
        let starts = vec![(seed.file.clone(), seed.position)];

        let all_affected = bfs::run_with_parents(
            &mut backend,
            starts.clone(),
            &entrypoints,
            BfsDirection::Upward,
        )?
        .affected;

        let mut classifier = CfgClassifier::new();
        let mut prod_backend = FilteredBackend {
            inner: &mut backend,
            classifier: &mut classifier,
        };
        let prod_affected = bfs::run_with_parents(
            &mut prod_backend,
            starts,
            &entrypoints,
            BfsDirection::Upward,
        )?
        .affected;

        let mut bins = BTreeMap::new();
        for bin in &all_bins {
            let r = if prod_affected.contains(bin) {
                Reachability::Production
            } else if all_affected.contains(bin) {
                Reachability::TestsOnly
            } else {
                Reachability::NotReachable
            };
            bins.insert(bin.clone(), r);
        }
        per_seed.push(SeedReachability { seed, bins });
    }

    lifecycle::shutdown(&mut client)?;
    Ok(RunResult { all_bins, per_seed })
}

struct FilteredBackend<'a, 'b> {
    inner: &'b mut Backend<'a>,
    classifier: &'b mut CfgClassifier,
}

impl FilteredBackend<'_, '_> {
    fn drop_test_gated(&mut self, locs: Vec<Location>) -> Vec<Location> {
        locs.into_iter()
            .filter(|loc| {
                !self
                    .classifier
                    .is_test_gated(&loc.path, loc.range.start)
                    .unwrap_or(false)
            })
            .collect()
    }
}

impl LspBackend for FilteredBackend<'_, '_> {
    fn open(&mut self, file: &Path) -> Result<()> {
        self.inner.open(file)
    }

    fn references(&mut self, file: &Path, pos: Position) -> Result<Vec<Location>> {
        let locs = self.inner.references(file, pos)?;
        Ok(self.drop_test_gated(locs))
    }

    fn references_batch(&mut self, items: &[(PathBuf, Position)]) -> Result<Vec<Vec<Location>>> {
        let results = self.inner.references_batch(items)?;
        Ok(results
            .into_iter()
            .map(|locs| self.drop_test_gated(locs))
            .collect())
    }

    fn declaration(&mut self, file: &Path, pos: Position) -> Result<(PathBuf, Position)> {
        self.inner.declaration(file, pos)
    }

    fn declarations_batch(
        &mut self,
        items: &[(PathBuf, Position)],
    ) -> Result<Vec<(PathBuf, Position)>> {
        self.inner.declarations_batch(items)
    }

    fn document_symbols(&mut self, file: &Path) -> Result<Vec<DocumentSymbol>> {
        self.inner.document_symbols(file)
    }

    fn document_symbols_batch(&mut self, files: &[PathBuf]) -> Result<Vec<Vec<DocumentSymbol>>> {
        self.inner.document_symbols_batch(files)
    }
}
