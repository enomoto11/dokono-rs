//! Exit-code policy.
//!
//! - `0`: analysis succeeded and no Production-reachable bin was found (and in default
//!   mode, no TestsOnly-reachable bin needs to fail the run either).
//! - `1`: analysis succeeded and at least one Production-reachable bin was found.
//!   In `--strict` mode this is also returned when only TestsOnly bins were found.
//! - `2`: tool error (raised at the `main` boundary; this module does not return 2).

use crate::runner::{Reachability, RunResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    ProductionReachable,
    TestsOnlyReachable,
}

pub fn verdict(run: &RunResult) -> Verdict {
    let mut has_prod = false;
    let mut has_test = false;
    for sr in &run.per_seed {
        for r in sr.bins.values() {
            match r {
                Reachability::Production => has_prod = true,
                Reachability::TestsOnly => has_test = true,
                Reachability::NotReachable => {}
            }
        }
    }
    if has_prod {
        Verdict::ProductionReachable
    } else if has_test {
        Verdict::TestsOnlyReachable
    } else {
        Verdict::Clean
    }
}

pub fn exit_code(verdict: Verdict, strict: bool) -> u8 {
    match verdict {
        Verdict::ProductionReachable => 1,
        Verdict::TestsOnlyReachable if strict => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::VulnSymbol;
    use crate::probe::ResolvedSeed;
    use crate::runner::SeedReachability;
    use dokono_core::types::Position;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    fn run_with(bins: &[(&str, Reachability)]) -> RunResult {
        let mut all_bins = BTreeSet::new();
        let mut bin_map = BTreeMap::new();
        for (path, r) in bins {
            let p = PathBuf::from(path);
            all_bins.insert(p.clone());
            bin_map.insert(p, *r);
        }
        RunResult {
            all_bins,
            per_seed: vec![SeedReachability {
                seed: ResolvedSeed {
                    symbol: VulnSymbol {
                        advisory_id: None,
                        crate_name: "x".into(),
                        path: "x::y".into(),
                        version_reqs: vec![],
                    },
                    file: PathBuf::from("/x"),
                    position: Position {
                        line: 0,
                        character: 0,
                    },
                },
                bins: bin_map,
            }],
        }
    }

    #[test]
    fn no_seeds_is_clean() {
        let r = RunResult {
            all_bins: BTreeSet::new(),
            per_seed: vec![],
        };
        assert_eq!(verdict(&r), Verdict::Clean);
        assert_eq!(exit_code(verdict(&r), false), 0);
        assert_eq!(exit_code(verdict(&r), true), 0);
    }

    #[test]
    fn all_not_reachable_is_clean() {
        let r = run_with(&[("/a", Reachability::NotReachable)]);
        assert_eq!(verdict(&r), Verdict::Clean);
        assert_eq!(exit_code(verdict(&r), false), 0);
        assert_eq!(exit_code(verdict(&r), true), 0);
    }

    #[test]
    fn production_reachable_exits_1() {
        let r = run_with(&[
            ("/a", Reachability::Production),
            ("/b", Reachability::NotReachable),
        ]);
        assert_eq!(verdict(&r), Verdict::ProductionReachable);
        assert_eq!(exit_code(verdict(&r), false), 1);
        assert_eq!(exit_code(verdict(&r), true), 1);
    }

    #[test]
    fn tests_only_default_is_0_strict_is_1() {
        let r = run_with(&[
            ("/a", Reachability::TestsOnly),
            ("/b", Reachability::NotReachable),
        ]);
        assert_eq!(verdict(&r), Verdict::TestsOnlyReachable);
        assert_eq!(exit_code(verdict(&r), false), 0);
        assert_eq!(exit_code(verdict(&r), true), 1);
    }

    #[test]
    fn mix_production_and_tests_is_production() {
        let r = run_with(&[
            ("/a", Reachability::Production),
            ("/b", Reachability::TestsOnly),
        ]);
        assert_eq!(verdict(&r), Verdict::ProductionReachable);
        assert_eq!(exit_code(verdict(&r), false), 1);
    }
}
