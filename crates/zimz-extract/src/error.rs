// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Core(#[from] zimz_core::Error),
    #[error("malformed JSON record: {0}")]
    Json(#[from] serde_json::Error),
    #[error("entry {0} is not extractable ({1})")]
    Unsupported(String, String),
    #[error("companion entry {0} not found")]
    Missing(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
