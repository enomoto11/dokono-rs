//! `initialize` → `initialized` and `shutdown` → `exit` LSP lifecycle helpers.

use anyhow::{anyhow, Context, Result};
use lsp_types::notification::Initialized;
use lsp_types::request::Initialize;
use lsp_types::{
    ClientCapabilities, DocumentSymbolClientCapabilities, InitializeParams, InitializedParams,
    ReferenceClientCapabilities, TextDocumentClientCapabilities,
    TextDocumentSyncClientCapabilities, Url, WindowClientCapabilities, WorkspaceFolder,
};
use serde_json::json;
use std::path::Path;

use crate::lsp::client::Client;

pub fn initialize(client: &mut Client, workspace: &Path) -> Result<()> {
    let abs = workspace
        .canonicalize()
        .with_context(|| format!("canonicalize failed for {}", workspace.display()))?;
    let root_uri = Url::from_directory_path(&abs)
        .map_err(|_| anyhow!("workspace is not an absolute path: {}", abs.display()))?;

    // `experimental.serverStatusNotification` enables rust-analyzer's
    // `experimental/serverStatus` notifications, which we use as the deterministic
    // readiness signal.
    let mut params = InitializeParams {
        process_id: Some(std::process::id()),
        capabilities: ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                synchronization: Some(TextDocumentSyncClientCapabilities {
                    did_save: Some(false),
                    will_save: Some(false),
                    will_save_wait_until: Some(false),
                    dynamic_registration: Some(false),
                }),
                document_symbol: Some(DocumentSymbolClientCapabilities {
                    hierarchical_document_symbol_support: Some(true),
                    ..Default::default()
                }),
                references: Some(ReferenceClientCapabilities {
                    dynamic_registration: Some(false),
                }),
                ..Default::default()
            }),
            window: Some(WindowClientCapabilities {
                work_done_progress: Some(true),
                ..Default::default()
            }),
            experimental: Some(json!({ "serverStatusNotification": true })),
            ..Default::default()
        },
        workspace_folders: Some(vec![WorkspaceFolder {
            uri: root_uri.clone(),
            name: "workspace".into(),
        }]),
        ..Default::default()
    };
    #[allow(deprecated)]
    {
        params.root_uri = Some(root_uri);
    }

    let _ = client
        .request::<Initialize>(params)
        .context("LSP initialize request failed")?;
    client
        .notify::<Initialized>(InitializedParams {})
        .context("LSP initialized notification failed")?;
    Ok(())
}

pub fn shutdown(client: &mut Client) -> Result<()> {
    client.shutdown_and_exit()
}
