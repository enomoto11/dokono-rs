use anyhow::{Context, Result};
use gix::diff::blob::{Algorithm, InternedInput, diff_with_slider_heuristics};
use gix::object::tree::diff::ChangeDetached;
use gix::remote::Direction;
use std::path::{Path, PathBuf};

pub fn fetch_pr(workspace: &Path, pr: u32, local_ref: &str) -> Result<()> {
    let repo = gix::open(workspace)
        .with_context(|| format!("failed to open git repo at {}", workspace.display()))?;
    let mut remote = repo
        .find_remote("origin")
        .context("failed to find remote 'origin'")?;
    let refspec = format!("+refs/pull/{pr}/head:refs/heads/{local_ref}");
    remote
        .replace_refspecs([refspec.as_str()], Direction::Fetch)
        .with_context(|| format!("failed to parse refspec {refspec}"))?;
    remote
        .connect(Direction::Fetch)
        .with_context(|| format!("failed to connect to origin for PR #{pr}"))?
        .prepare_fetch(gix::progress::Discard, Default::default())
        .with_context(|| format!("failed to prepare fetch for PR #{pr}"))?
        .receive(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
        .with_context(|| format!("git fetch origin {refspec} failed"))?;
    Ok(())
}

pub fn resolve_sha(workspace: &Path, rev: &str) -> Result<String> {
    let repo = gix::open(workspace)
        .with_context(|| format!("failed to open git repo at {}", workspace.display()))?;
    let id = repo
        .rev_parse_single(rev)
        .with_context(|| format!("failed to resolve revision {rev}"))?
        .detach();
    Ok(id.to_string())
}

pub fn merge_base(workspace: &Path, a: &str, b: &str) -> Result<String> {
    let repo = gix::open(workspace)
        .with_context(|| format!("failed to open git repo at {}", workspace.display()))?;
    let one = repo
        .rev_parse_single(a)
        .with_context(|| format!("failed to resolve revision {a}"))?
        .detach();
    let two = repo
        .rev_parse_single(b)
        .with_context(|| format!("failed to resolve revision {b}"))?
        .detach();
    let base = repo
        .merge_base(one, two)
        .with_context(|| format!("merge-base of {a} and {b} not found"))?;
    Ok(base.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub lines: Vec<u32>,
}

/// Compute the set of changed `.rs` files between two revisions and the new-side line numbers.
/// Line numbers are 1-based to match git/LSP conventions on the consumer side; LSP is 0-based,
/// so callers convert.
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
