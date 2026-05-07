use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command as ProcCommand;

pub fn fetch_pr(workspace: &Path, pr: u32, local_ref: &str) -> Result<()> {
    let refspec = format!("pull/{pr}/head:{local_ref}");
    let status = ProcCommand::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["fetch", "origin"])
        .arg(&refspec)
        .status()
        .with_context(|| format!("failed to spawn git fetch for PR #{pr}"))?;
    if !status.success() {
        anyhow::bail!("git fetch origin {refspec} failed (status {status})");
    }
    Ok(())
}

pub fn merge_base(workspace: &Path, a: &str, b: &str) -> Result<String> {
    let output = ProcCommand::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["merge-base", a, b])
        .output()
        .context("failed to spawn git merge-base")?;
    if !output.status.success() {
        anyhow::bail!(
            "git merge-base {a} {b} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim(),
        );
    }
    let sha = String::from_utf8(output.stdout)
        .context("git merge-base output not UTF-8")?
        .trim()
        .to_string();
    if sha.is_empty() {
        anyhow::bail!("git merge-base returned empty result for {a} and {b}");
    }
    Ok(sha)
}
