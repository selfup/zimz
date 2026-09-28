// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

use std::borrow::Cow;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("not a Xapian glass database (bad magic)")]
    BadMagic,
    #[error("unsupported glass format version {0:#06x} (expected 0x046e = 2016-03-14)")]
    UnsupportedVersion(u16),
    #[error("corrupt glass database: {0}")]
    Corrupt(Cow<'static, str>),
    #[error("tag decompression failed: {0}")]
    Inflate(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn corrupt(msg: impl Into<Cow<'static, str>>) -> Self {
        Error::Corrupt(msg.into())
    }
}
