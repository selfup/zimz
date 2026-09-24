// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Archive and cache diagnostics.

use serde::{Deserialize, Serialize};
use zimz_core::integrity::{self, Check};

use crate::library::{ExtractCacheStats, Library, ScanFailure};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Verify {
    /// Report what is known without reading the archive (default).
    #[default]
    None,
    /// Structural checks: pointer tables, ordering, title index, MIME types (reads
    /// every directory entry; seconds on very large archives).
    Quick,
    /// Quick checks plus the MD5 checksum and decoding of every cluster (reads the whole
    /// file; minutes for a 100 GB archive).
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HealthRequest {
    /// One archive (name, uuid, file stem or glob); empty = every archive.
    #[serde(default)]
    pub archive: Option<String>,
    #[serde(default)]
    pub verify: Verify,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CacheStats {
    pub hits: usize,
    pub misses: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArchiveHealth {
    pub name: String,
    pub file: String,
    pub uuid: String,
    pub size_bytes: u64,
    /// Milliseconds spent opening and probing the archive at startup.
    pub open_ms: f64,
    pub article_count: u32,
    pub has_fulltext_index: bool,
    pub fulltext_docs: Option<u32>,
    /// `fulltext_docs / article_count`; well below 1.0 means many articles are not
    /// searchable by body text (typical for app-style archives).
    pub fulltext_coverage: Option<f64>,
    pub has_title_index: bool,
    pub title_docs: Option<u32>,
    pub has_title_listing: bool,
    /// `ok`, `mismatch`, `none` (archive has no checksum) or `skipped`.
    pub checksum: String,
    /// Problems found by the requested verification.
    pub problems: Vec<String>,
    pub cluster_cache: CacheStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HealthResponse {
    pub archives: Vec<ArchiveHealth>,
    /// Files that failed to open during the scan.
    pub failures: Vec<ScanFailure>,
    pub extract_cache: ExtractCacheStats,
    pub scan_ms: f64,
}

impl Library {
    #[allow(clippy::cast_precision_loss)]
    pub fn health(&self, req: &HealthRequest) -> Result<HealthResponse> {
        let selection = match &req.archive {
            Some(a) if !a.trim().is_empty() => self.select(std::slice::from_ref(a))?,
            _ => (0..self.len()).collect(),
        };
        if selection.is_empty() {
            return Err(Error::Empty);
        }
        let mut archives = Vec::with_capacity(selection.len());
        for i in selection {
            let slot = &self.slots()[i];
            let archive = slot.archive();
            let info = &slot.info;
            let mut problems = Vec::new();
            let mut checksum = "skipped".to_string();
            match req.verify {
                Verify::None => {}
                Verify::Quick => {
                    for p in integrity::run(archive, &Check::QUICK) {
                        problems.push(format!("{:?}: {}", p.check, p.message));
                    }
                }
                Verify::Full => {
                    let mut checks = Check::QUICK.to_vec();
                    checks.push(Check::Clusters);
                    for p in integrity::run(archive, &checks) {
                        problems.push(format!("{:?}: {}", p.check, p.message));
                    }
                    checksum = match archive.verify_checksum() {
                        Ok(true) => "ok".into(),
                        Ok(false) => {
                            problems.push("Checksum: MD5 mismatch".into());
                            "mismatch".into()
                        }
                        Err(zimz_core::Error::NoChecksum) => "none".into(),
                        Err(e) => {
                            problems.push(format!("Checksum: {e}"));
                            "error".into()
                        }
                    };
                }
            }
            let (hits, misses) = archive.cluster_cache_stats();
            let coverage = info.fulltext_docs.and_then(|d| {
                (info.article_count > 0).then(|| f64::from(d) / f64::from(info.article_count))
            });
            archives.push(ArchiveHealth {
                name: info.name.clone(),
                file: info.file.clone(),
                uuid: info.uuid.clone(),
                size_bytes: info.size_bytes,
                open_ms: slot.open_time().as_secs_f64() * 1000.0,
                article_count: info.article_count,
                has_fulltext_index: info.has_fulltext_index,
                fulltext_docs: info.fulltext_docs,
                fulltext_coverage: coverage.map(|c| (c * 1000.0).round() / 1000.0),
                has_title_index: info.has_title_index,
                title_docs: info.title_docs,
                has_title_listing: slot.has_listing(),
                checksum,
                problems,
                cluster_cache: CacheStats { hits, misses },
            });
        }
        Ok(HealthResponse {
            archives,
            failures: self.failures().to_vec(),
            extract_cache: self.extract_cache_stats(),
            scan_ms: self.scan_time().as_secs_f64() * 1000.0,
        })
    }
}
