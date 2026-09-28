// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! `zimz-extract` turns ZIM entries into agent-friendly text: Markdown with an outline
//! and resolved links, plain text for snippets and word counts. A per-scraper adapter
//! prunes navigation, references and other boilerplate, and reads the JSON records that
//! app-style scrapers (LibreTexts, YouTube) store instead of HTML.
#![allow(clippy::doc_markdown)]

pub mod adapter;
pub mod document;
pub mod error;
pub mod links;
pub mod render;
pub mod snippet;
pub mod vtt;

pub use adapter::{Adapter, detect_adapter, extract, extract_html};
pub use document::{Document, Link, OutlineEntry, Section};
pub use error::{Error, Result};
pub use links::LinkTarget;
pub use render::RenderOptions;
pub use snippet::{Snippet, best_snippet, best_snippet_with};
