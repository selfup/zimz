// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Core(#[from] zimz_core::Error),
    #[error(transparent)]
    Glass(#[from] zimz_glass::Error),
    #[error(transparent)]
    Extract(#[from] zimz_extract::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("no archive matches {0:?}")]
    NoSuchArchive(String),
    #[error("the library contains no archives")]
    Empty,
    #[error("entry {path:?} not found in archive {archive}")]
    NoSuchEntry { archive: String, path: String },
    #[error("invalid or stale cursor")]
    BadCursor,
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
