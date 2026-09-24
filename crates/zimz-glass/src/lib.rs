// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! `zimz-glass` reads the Xapian "glass" single-file databases that libzim embeds in
//! ZIM archives (`X/fulltext/xapian`, `X/title/xapian`) without linking Xapian, and
//! evaluates queries over them with Xapian's BM25 weighting.
//!
//! The on-disk format (block B-trees, posting-list chunks, sortable varints) is ported
//! from xapian-core 1.4 (GPL-2.0-or-later), which is licence-compatible with this crate.
//! Only what libzim writes is supported: read-only, format version 2016-03-14, no
//! termlist table, optional positions (title index only).

pub mod analyzer;
pub mod db;
pub mod error;
pub mod pack;
pub mod position;
pub mod postlist;
pub mod search;
pub mod suggest;
pub mod table;
pub mod version;

pub use analyzer::Analyzer;
pub use db::GlassDb;
pub use error::{Error, Result};
pub use postlist::PostList;
pub use search::{Hit, Op, Query, SearchResults, search};
