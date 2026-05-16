//! Compute the set of changed `.rs` files between two revisions and the new-side line numbers.
//! Line numbers are 1-based to match git/LSP conventions on the consumer side; LSP is 0-based,
//! so callers convert.

use anyhow::{Context, Result};
use gix::diff::blob::{Algorithm, InternedInput, diff_with_slider_heuristics};
use gix::object::tree::diff::ChangeDetached;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub lines: Vec<u32>,
}

pub fn changes_between(workspace: &Path, base: &str, head: &str) -> Result<Vec<ChangedFile>> {
    let repo = gix::open(workspace)
        .with_context(|| format!("failed to open git repo at {}", workspace.display()))?;
    let from = tree_for_rev(&repo, base)?;
    let to = tree_for_rev(&repo, head)?;
    let changes = repo
        .diff_tree_to_tree(Some(&from), Some(&to), None)
        .with_context(|| format!("git diff {base}..{head} failed"))?;

    let mut out = Vec::new();
    for change in changes {
        if let Some(file) = changed_file_from(&repo, change)? {
            out.push(file);
        }
    }
    Ok(out)
}

fn tree_for_rev<'repo>(repo: &'repo gix::Repository, rev: &str) -> Result<gix::Tree<'repo>> {
    repo.rev_parse_single(rev)
        .with_context(|| format!("failed to resolve revision {rev}"))?
        .object()
        .with_context(|| format!("failed to read object for {rev}"))?
        .peel_to_tree()
        .with_context(|| format!("{rev} does not point to a tree-ish"))
}

fn changed_file_from(
    repo: &gix::Repository,
    change: ChangeDetached,
) -> Result<Option<ChangedFile>> {
    let (location, previous_id, new_id) = match change {
        ChangeDetached::Deletion { .. } => return Ok(None),
        ChangeDetached::Addition { location, id, .. } => (location, None, id),
        ChangeDetached::Modification {
            location,
            previous_id,
            id,
            ..
        } => (location, Some(previous_id), id),
        ChangeDetached::Rewrite {
            location,
            source_id,
            id,
            ..
        } => (location, Some(source_id), id),
    };

    let path = gix::path::try_from_bstring(location).context("non-UTF-8 path in tree diff")?;
    if path.extension().and_then(|s| s.to_str()) != Some("rs") {
        return Ok(None);
    }

    let new_data = repo
        .find_object(new_id)
        .with_context(|| format!("failed to read new blob for {}", path.display()))?
        .data
        .clone();
    let prev_data = match previous_id {
        Some(id) => repo
            .find_object(id)
            .with_context(|| format!("failed to read previous blob for {}", path.display()))?
            .data
            .clone(),
        None => Vec::new(),
    };

    let lines = added_lines(&prev_data, &new_data);
    if lines.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ChangedFile { path, lines }))
    }
}

fn added_lines(before: &[u8], after: &[u8]) -> Vec<u32> {
    let input = InternedInput::new(before, after);
    let diff = diff_with_slider_heuristics(Algorithm::Histogram, &input);
    let mut out = Vec::new();
    for hunk in diff.hunks() {
        for n in hunk.after.start..hunk.after.end {
            out.push(n + 1);
        }
    }
    out
}
