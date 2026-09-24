// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot
#![allow(dead_code)]

pub mod builder;

use std::path::{Path, PathBuf};
use zimz_core::source::MemorySource;
use zimz_core::{Archive, OpenConfig};

/// Open in-memory bytes as an archive.
pub fn open_bytes(bytes: Vec<u8>) -> zimz_core::Result<Archive> {
    Archive::from_source(Box::new(MemorySource::new(bytes)))
}

pub fn open_bytes_with(bytes: Vec<u8>, config: OpenConfig) -> zimz_core::Result<Archive> {
    Archive::from_source_with(Box::new(MemorySource::new(bytes)), config)
}

/// `fixtures/zim-testing-suite/data`, if the fixtures have been fetched.
pub fn fixtures_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/zim-testing-suite/data");
    if dir.is_dir() {
        Some(dir)
    } else {
        eprintln!(
            "skipping: fixtures not found at {} (run scripts/fetch-fixtures.sh)",
            dir.display()
        );
        None
    }
}

/// The directory named by `ZIMZ_TEST_ZIM_DIR`, if set and present.
pub fn local_library() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("ZIMZ_TEST_ZIM_DIR")?);
    if dir.is_dir() {
        Some(dir)
    } else {
        eprintln!(
            "skipping: ZIMZ_TEST_ZIM_DIR={} is not a directory",
            dir.display()
        );
        None
    }
}

pub fn zim_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("readable dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "zim"))
        .collect();
    v.sort();
    v
}

pub fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    use std::fmt::Write as _;
    let mut h = Md5::new();
    h.update(bytes);
    let digest: [u8; 16] = h.finalize().into();
    digest.iter().fold(String::with_capacity(32), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
