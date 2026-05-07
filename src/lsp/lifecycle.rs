//! `initialize` → `initialized` and `shutdown` → `exit` LSP lifecycle helpers.

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::path::Path;
use url::Url;

use crate::lsp::client::Client;

pub fn initialize(client: &mut Client, workspace: &Path) -> Result<()> {
    let abs = workspace
        .canonicalize()
        .with_context(|| format!("canonicalize failed for {}", workspace.display()))?;
    let root_uri = Url::from_directory_path(&abs)
        .map_err(|_| anyhow!("workspace is not an absolute path: {}", abs.display()))?;

    // `experimental.serverStatusNotification` enables rust-analyzer's `experimental/serverStatus`
    // notifications, which we use as the deterministic readiness signal.
    let params = json!({
        "processId": std::process::id(),
        "rootUri": root_uri,
        "capabilities": {
            "textDocument": {
                "synchronization": { "didOpen": true },
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                "references": {}
            },
            "window": {
                "workDoneProgress": true
            },
            "experimental": {
                "serverStatusNotification": true
            }
        },
        "workspaceFolders": [{
            "uri": root_uri,
            "name": "workspace"
        }]
    });

    let _result: Value = client
        .request("initialize", params)
        .context("LSP initialize request failed")?;
    client
        .notify("initialized", json!({}))
        .context("LSP initialized notification failed")?;
    Ok(())
}

pub fn shutdown(client: &mut Client) -> Result<()> {
    let _: Value = client
        .request("shutdown", Value::Null)
        .context("LSP shutdown request failed")?;
    client
        .notify("exit", Value::Null)
        .context("LSP exit notification failed")?;
    Ok(())
}
