// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! `zimz-core` reads ZIM archives (the openZIM file format used by Kiwix) without
//! libzim: header, MIME list, directory entries, clusters (none / zstd / xz), title
//! listings, metadata, checksum, split (`.zimaa`) and embedded archives.
//!
//! The entry point is [`Archive`]. Everything is read lazily from a memory-mapped
//! [`source::Source`]; decompressed clusters and parsed directory entries are cached.
//!
//! The format handling follows the `openZIM` specification and libzim's behaviour
//! (lookup rules, redirect resolution, embedded index location), whose sources were
//! consulted while writing this crate; libzim is copyright its authors (the `openZIM`
//! project), GPL-2.0-or-later, used here under GPL-3.0. See the repository `COPYRIGHT`.

pub mod archive;
mod cache;
pub mod cluster;
mod codec;
pub mod dirent;
pub mod error;
pub mod header;
pub mod integrity;
pub mod metadata;
pub mod mime;
pub mod source;
pub mod title;

pub use archive::{Archive, Blob, DirectAccess, OpenConfig};
pub use cluster::{ClusterData, Compression};
pub use dirent::{Dirent, DirentKind};
pub use error::{Error, Result};
pub use header::Header;
pub use mime::MimeList;
pub use title::TitleIndex;
