//! High-level LSP operations invoked by `dokono-rs debug` subcommands.
//!
//! Centralises `lsp_types` usage so the binary entrypoint and CLI dispatcher
//! stay independent of the LSP wire types.

use anyhow::{Context, Result};
use lsp_types::request::{DocumentSymbolRequest, References};
use lsp_types::{DocumentSymbol, DocumentSymbolResponse, Location, Position};
use std::path::Path;
use std::time::Instant;

use crate::analysis::symbols;
use crate::lsp::backend::{
    document_symbol_params, open_document, parse_document_symbols, references_params,
};
use crate::lsp::{client::Client, lifecycle, progress};

pub fn spawn_only(workspace: &Path) -> Result<()> {
    let client = Client::spawn(workspace)?;
    match client.pid() {
        Some(pid) => println!("rust-analyzer spawned: pid={pid}"),
        None => println!("rust-analyzer spawned (pid unavailable)"),
    }
    println!("dropping client (will kill the process)...");
    drop(client);
    println!("done");
    Ok(())
}

pub fn index(workspace: &Path) -> Result<()> {
    let mut client = Client::spawn(workspace)?;
    tracing::info!(
        "rust-analyzer spawned: pid={}",
        client.pid().unwrap_or_default()
    );

    let init_start = Instant::now();
    lifecycle::initialize(&mut client, workspace)?;
    tracing::info!("initialize ok ({:.2?})", init_start.elapsed());

    tracing::info!("waiting for index end ...");
    let idx_start = Instant::now();
    progress::wait_for_index_end(&client)?;
    tracing::info!("index ended ({:.2?})", idx_start.elapsed());

    lifecycle::shutdown(&mut client)?;
    tracing::info!("shutdown ok");
    Ok(())
}

pub fn symbols(workspace: &Path, file: &Path, line: Option<u32>) -> Result<()> {
    let file_abs = workspace
        .join(file)
        .canonicalize()
        .with_context(|| format!("file not found: {}", file.display()))?;

    let mut client = bootstrap_client(workspace)?;
    let symbols = fetch_document_symbols(&client, &file_abs)?;

    match line {
        Some(line1) => {
            let hits = symbols::pick_at_lines(&symbols, &[line1]);
            if hits.is_empty() {
                println!("no symbol hits line {line1}");
            } else {
                for h in &hits {
                    println!(
                        "hit: {} @ ({}, {})",
                        h.name, h.position.line, h.position.character
                    );
                }
            }
        }
        None => {
            println!("documentSymbol tree:");
            print_symbol_tree(&symbols, 0);
        }
    }

    lifecycle::shutdown(&mut client)?;
    Ok(())
}

pub fn references(workspace: &Path, file: &Path, line: u32, character: u32) -> Result<()> {
    let file_abs = workspace
        .join(file)
        .canonicalize()
        .with_context(|| format!("file not found: {}", file.display()))?;

    let mut client = bootstrap_client(workspace)?;
    open_document(&client, &file_abs)?;

    let pos = Position { line, character };
    let response: Option<Vec<Location>> =
        client.request::<References>(references_params(&file_abs, pos)?)?;
    let refs = response.unwrap_or_default();
    if refs.is_empty() {
        println!(
            "(no references found at {}:{}:{})",
            file.display(),
            line,
            character
        );
    } else {
        println!("references ({} hits):", refs.len());
        for r in &refs {
            let display = r
                .uri
                .to_file_path()
                .ok()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| r.uri.to_string());
            println!(
                "  {}:{}:{}",
                display, r.range.start.line, r.range.start.character
            );
        }
    }

    lifecycle::shutdown(&mut client)?;
    Ok(())
}

fn bootstrap_client(workspace: &Path) -> Result<Client> {
    let mut client = Client::spawn(workspace)?;
    tracing::info!(
        "rust-analyzer spawned: pid={}",
        client.pid().unwrap_or_default()
    );
    lifecycle::initialize(&mut client, workspace)?;
    tracing::info!("waiting for index end ...");
    progress::wait_for_index_end(&client)?;
    tracing::info!("index ended");
    Ok(client)
}

fn fetch_document_symbols(client: &Client, file_abs: &Path) -> Result<Vec<DocumentSymbol>> {
    open_document(client, file_abs)?;
    let response: Option<DocumentSymbolResponse> =
        client.request::<DocumentSymbolRequest>(document_symbol_params(file_abs)?)?;
    parse_document_symbols(response, file_abs)
}

fn print_symbol_tree(symbols: &[DocumentSymbol], depth: usize) {
    let indent = "  ".repeat(depth);
    for s in symbols {
        println!(
            "{indent}- {} ({:?}) range=({},{})..({},{}) sel=({},{})",
            s.name,
            s.kind,
            s.range.start.line,
            s.range.start.character,
            s.range.end.line,
            s.range.end.character,
            s.selection_range.start.line,
            s.selection_range.start.character,
        );
        if let Some(children) = &s.children {
            print_symbol_tree(children, depth + 1);
        }
    }
}
