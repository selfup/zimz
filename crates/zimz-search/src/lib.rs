// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! `zimz-search` turns a directory of ZIM archives into one searchable library: it
//! catalogues every archive, fans a query out to each archive's embedded Xapian index
//! (or its title listing when there is none), fuses the per-archive rankings with
//! reciprocal rank fusion, and serves articles, outlines, links and budgeted context
//! excerpts through [`zimz_extract`].
//!
//! Every request/response type is plain data (`serde`, optionally `schemars`) so the
//! MCP server and the CLI share one implementation.
#![allow(clippy::doc_markdown)]

pub mod article;
pub mod catalog;
pub mod context;
pub mod error;
pub mod fusion;
pub mod health;
pub mod library;
pub mod search;
pub mod suggest;

pub use article::{
    ArticleRequest, ArticleResponse, Format, LinkInfo, LinksRequest, LinksResponse, OutlineItem,
    OutlineResponse,
};
pub use catalog::{ArchiveInfo, Mode};
pub use context::{ContextRequest, ContextResponse, Excerpt, render_markdown};
pub use error::{Error, Result};
pub use health::{ArchiveHealth, CacheStats, HealthRequest, HealthResponse, Verify};
pub use library::{
    ExtractCacheStats, Library, LibraryConfig, Resolved, ScanFailure, Slot, discover_files,
};
pub use search::{ModeSelect, SearchHit, SearchRequest, SearchResponse};
pub use suggest::{SuggestRequest, SuggestResponse, SuggestionHit};
