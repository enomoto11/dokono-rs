use anyhow::Result;

use crate::lsp::client::Client;

/// Wait until rust-analyzer reports `quiescent: true` via `experimental/serverStatus`.
pub fn wait_for_index_end(client: &Client) -> Result<()> {
    client.wait_for_quiescent_after(0)?;
    Ok(())
}
