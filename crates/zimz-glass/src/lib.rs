// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot
//
// Portions ported from xapian-core 1.4 and libzim (GPL-2.0-or-later, used under
// GPL-3.0 as those licences permit):
//   Copyright 1999,2000,2001 BrightStation PLC
//   Copyright 2002 Ananova Ltd
//   Copyright 2002-2025 Olly Betts
//   Copyright 2008 Lemur Consulting Ltd
//   and other Xapian contributors; Copyright the libzim authors (openZIM project).
// See the repository COPYRIGHT file.

//! `zimz-glass` reads the Xapian "glass" single-file databases that libzim embeds in
//! ZIM archives (`X/fulltext/xapian`, `X/title/xapian`) without linking Xapian, and
//! evaluates queries over them with Xapian's BM25 weighting.
//!
//! The on-disk format (block B-trees, posting-list chunks, sortable varints), the BM25
//! weighting and the suggestion semantics are ported from xapian-core 1.4 and libzim
//! (both GPL-2.0-or-later, used here under GPL-3.0 as those licences permit); the
//! original copyright notices are at the top of this file and in the repository
//! `COPYRIGHT` file. Only what libzim writes is supported: read-only, format version
//! 2016-03-14, no termlist table, optional positions (title index only).

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
