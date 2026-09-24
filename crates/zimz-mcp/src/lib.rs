// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! `zimz-mcp` serves a [`zimz_search::Library`] over the Model Context Protocol so
//! agents can search a directory of ZIM archives and read articles from them. Only the
//! stdio transport is wired up; the tool surface is transport-agnostic.
#![allow(clippy::doc_markdown)]

pub mod render;
pub mod server;

use std::sync::Arc;

use rmcp::ServiceExt;
pub use server::{INSTRUCTIONS, ZimServer};
use zimz_search::Library;

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Log to stderr (stdout carries the protocol). Level from `ZIMZ_LOG`, then
/// `RUST_LOG`, default `info`.
pub fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("ZIMZ_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false)
        .try_init();
}

/// Serve `library` on stdin/stdout until the client disconnects.
pub fn run_stdio(library: Library) -> Result<(), BoxError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let server = ZimServer::new(Arc::new(library));
        tracing::info!(
            archives = server.library().len(),
            failures = server.library().failures().len(),
            "zimz MCP server ready on stdio"
        );
        let running = server.serve(rmcp::transport::stdio()).await?;
        let reason = running.waiting().await?;
        tracing::info!(?reason, "zimz MCP server stopped");
        Ok(())
    })
}
