use anyhow::{Context, Result};
use gix::remote::Direction;
use std::path::Path;

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
