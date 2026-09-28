// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

use std::borrow::Cow;

/// Errors produced while opening or reading a ZIM archive.
///
/// Malformed input never panics; it surfaces as [`Error::Corrupt`] or one of the more
/// specific variants.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a ZIM file (bad magic number)")]
    BadMagic,
    #[error("unsupported ZIM major version {0} (expected 5 or 6)")]
    UnsupportedVersion(u16),
    #[error("unsupported cluster compression code {0}")]
    UnsupportedCompression(u8),
    #[error("corrupt ZIM file: {0}")]
    Corrupt(Cow<'static, str>),
    #[error("decompression failed: {0}")]
    Decode(String),
    #[error("cluster of {size} bytes exceeds the configured limit of {limit} bytes")]
    ClusterTooLarge { size: u64, limit: u64 },
    #[error("read of {len} bytes at offset {offset} is outside the archive (size {size})")]
    OutOfBounds { offset: u64, len: u64, size: u64 },
    #[error("redirect chain longer than {0} hops")]
    RedirectLoop(u32),
    #[error("entry {0} is not an item (it is a redirect or a placeholder)")]
    NotAnItem(u32),
    #[error("archive has no title index")]
    NoTitleIndex,
    #[error("archive has no checksum")]
    NoChecksum,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn corrupt(msg: impl Into<Cow<'static, str>>) -> Self {
        Error::Corrupt(msg.into())
    }
}
