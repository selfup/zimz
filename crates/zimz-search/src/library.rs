// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! The library: every archive found under the configured directories, opened once and
//! shared, plus the caches that make repeated agent calls cheap.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lru::LruCache;
use percent_encoding::percent_decode_str;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use zimz_core::{Archive, Dirent, OpenConfig};
use zimz_extract::{Adapter, Document};
use zimz_glass::{Analyzer, GlassDb};

use crate::catalog::{ArchiveInfo, Mode, file_stem_name};
use crate::{Error, Result};

/// Where to look and how much memory to spend.
#[derive(Debug, Clone)]
pub struct LibraryConfig {
    /// Directories scanned for `*.zim` (and split `*.zimaa`) files.
    pub dirs: Vec<PathBuf>,
    /// Individual files added on top of the directory scan.
    pub files: Vec<PathBuf>,
    /// Descend into subdirectories.
    pub recursive: bool,
    /// Total byte budget for decoded clusters, shared evenly across archives.
    pub cluster_cache_bytes: usize,
    /// Byte budget for extracted documents (Markdown + text), shared by all archives.
    pub extract_cache_bytes: usize,
    /// `(glob, weight)` ranking priorities matched against archive names; first match
    /// wins, unmatched archives get 1.0.
    pub priorities: Vec<(String, f64)>,
    /// Snippets and context excerpts are extracted from at most this many bytes of an
    /// item (whole Gutenberg books are tens of MB); `read_article` is not capped.
    pub max_snippet_source_bytes: usize,
}

impl Default for LibraryConfig {
    fn default() -> Self {
        Self {
            dirs: Vec::new(),
            files: Vec::new(),
            recursive: true,
            cluster_cache_bytes: 256 << 20,
            extract_cache_bytes: 64 << 20,
            priorities: Vec::new(),
            max_snippet_source_bytes: 4 << 20,
        }
    }
}

/// An archive that could not be opened during the scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ScanFailure {
    pub file: String,
    pub error: String,
}

/// Location and layout of one embedded Xapian database.
#[derive(Debug, Clone)]
pub(crate) struct IndexRef {
    pub offset: u64,
    pub len: u64,
    pub doc_count: u32,
    pub title_slot: u32,
    pub wordcount_slot: Option<u32>,
    pub language: Option<String>,
}

impl IndexRef {
    fn probe(archive: &Archive, da: zimz_core::DirectAccess) -> Result<Self> {
        let bytes = archive.source().slice(da.offset, da.len as usize)?;
        let db = GlassDb::open(&bytes)?;
        let map = db.valuesmap().unwrap_or_default();
        Ok(Self {
            offset: da.offset,
            len: da.len,
            doc_count: db.doc_count(),
            title_slot: map.get("title").copied().unwrap_or(0),
            wordcount_slot: map.get("wordcount").copied(),
            language: db.metadata_string("language").ok().flatten(),
        })
    }
}

/// One opened archive with everything derived from it once.
pub struct Slot {
    pub info: ArchiveInfo,
    archive: Arc<Archive>,
    analyzer: Analyzer,
    adapter: Adapter,
    fulltext: Option<IndexRef>,
    title_index: Option<IndexRef>,
    open_time: Duration,
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot")
            .field("name", &self.info.name)
            .field("file", &self.info.file)
            .finish_non_exhaustive()
    }
}

impl Slot {
    fn open(file: &Path, cluster_cache_bytes: usize) -> Result<Self> {
        let t0 = Instant::now();
        let config = OpenConfig {
            cluster_cache_bytes,
            ..OpenConfig::default()
        };
        let archive = Archive::open_with(file, config)?;
        let adapter = zimz_extract::detect_adapter(&archive);
        let mut info = ArchiveInfo::read(&archive, file, &format!("{adapter:?}"))?;
        let fulltext = match archive.fulltext_index()? {
            Some(da) => Some(IndexRef::probe(&archive, da)?),
            None => None,
        };
        let title_index = match archive.title_xapian_index()? {
            Some(da) => Some(IndexRef::probe(&archive, da)?),
            None => None,
        };
        info.fulltext_docs = fulltext.as_ref().map(|i| i.doc_count);
        info.title_docs = title_index.as_ref().map(|i| i.doc_count);
        let language = fulltext
            .as_ref()
            .or(title_index.as_ref())
            .and_then(|i| i.language.clone())
            .or_else(|| info.language.clone());
        // Multi-language archives list `eng,fra`; the indexer used the first.
        let first_lang = language
            .as_deref()
            .map(|l| l.split(',').next().unwrap_or(l).trim().to_string());
        Ok(Self {
            info,
            archive: Arc::new(archive),
            analyzer: Analyzer::new(first_lang.as_deref()),
            adapter,
            fulltext,
            title_index,
            open_time: t0.elapsed(),
        })
    }

    pub fn name(&self) -> &str {
        &self.info.name
    }

    pub fn archive(&self) -> &Arc<Archive> {
        &self.archive
    }

    pub fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }

    pub fn adapter(&self) -> Adapter {
        self.adapter
    }

    /// Time spent opening and probing the archive during the scan.
    pub fn open_time(&self) -> Duration {
        self.open_time
    }

    pub fn has_fulltext(&self) -> bool {
        self.fulltext.is_some()
    }

    pub fn has_title_index(&self) -> bool {
        self.title_index.is_some()
    }

    pub fn has_listing(&self) -> bool {
        !matches!(self.archive.title_index(), zimz_core::TitleIndex::None)
    }

    /// `zim://name/path`.
    pub fn uri(&self, path: &str) -> String {
        format!("zim://{}/{}", self.info.name, path)
    }

    /// The prefix given to `zimz-extract` so Markdown links become `zim://` URIs.
    pub fn link_prefix(&self) -> String {
        format!("zim://{}/", self.info.name)
    }

    /// The title stored in the directory for a user path (the title index sometimes
    /// stores a lowercased copy).
    pub fn entry_title(&self, path: &str) -> Option<String> {
        let d = self.archive.entry_by_path_compat(path).ok().flatten()?;
        let t = d.title();
        (!t.is_empty()).then(|| t.to_string())
    }

    /// The user-facing path for a document-data string of the embedded index
    /// (`C/Foo` → `Foo` under the new scheme; `A/Foo` stays as is under the old one).
    pub fn user_path_from_index(&self, data: &str) -> String {
        if self.archive.uses_new_namespace_scheme() {
            data.strip_prefix("C/").unwrap_or(data).to_string()
        } else {
            data.to_string()
        }
    }

    pub(crate) fn with_fulltext<R>(
        &self,
        f: impl FnOnce(&GlassDb<'_>, &IndexRef) -> Result<R>,
    ) -> Result<Option<R>> {
        self.with_index(self.fulltext.as_ref(), f)
    }

    pub(crate) fn with_title_index<R>(
        &self,
        f: impl FnOnce(&GlassDb<'_>, &IndexRef) -> Result<R>,
    ) -> Result<Option<R>> {
        self.with_index(self.title_index.as_ref(), f)
    }

    fn with_index<R>(
        &self,
        index: Option<&IndexRef>,
        f: impl FnOnce(&GlassDb<'_>, &IndexRef) -> Result<R>,
    ) -> Result<Option<R>> {
        let Some(ix) = index else {
            return Ok(None);
        };
        let bytes: Cow<'_, [u8]> = self.archive.source().slice(ix.offset, ix.len as usize)?;
        let db = GlassDb::open(&bytes)?;
        f(&db, ix).map(Some)
    }

    /// Resolve an agent-supplied path: `zim://name/path`, `C/path`, `A/path`, a bare
    /// path (percent-encoded or not, `_` or spaces), or a title. Redirects are followed.
    pub fn resolve_entry(&self, path: &str) -> Result<Resolved> {
        let raw = strip_uri(path.trim());
        let raw = raw.split('#').next().unwrap_or(raw);
        let raw = raw.trim_start_matches('/');
        if raw.is_empty()
            && let Some(main) = self.archive.main_entry()?
        {
            return self.finish(&main, raw);
        }
        let decoded = percent_decode_str(raw).decode_utf8_lossy().into_owned();
        let mut candidates: Vec<String> = vec![raw.to_string()];
        if decoded != raw {
            candidates.push(decoded.clone());
        }
        let underscored = decoded.replace(' ', "_");
        if underscored != decoded {
            candidates.push(underscored);
        }
        for c in &candidates {
            if let Some(d) = self.archive.entry_by_path_compat(c)? {
                return self.finish(&d, raw);
            }
            if let Some(d) = self.archive.entry_by_long_path(c)? {
                return self.finish(&d, raw);
            }
        }
        for c in &candidates {
            if let Some(d) = self.archive.entry_by_title(c)? {
                return self.finish(&d, raw);
            }
            let spaced = c.replace('_', " ");
            if spaced != *c
                && let Some(d) = self.archive.entry_by_title(&spaced)?
            {
                return self.finish(&d, raw);
            }
        }
        Err(Error::NoSuchEntry {
            archive: self.info.name.clone(),
            path: path.to_string(),
        })
    }

    fn finish(&self, found: &Arc<Dirent>, requested: &str) -> Result<Resolved> {
        let target = self.archive.resolve(found)?;
        let redirected_from = (target.index != found.index).then(|| self.archive.user_path(found));
        let path = self.archive.user_path(&target);
        let _ = requested;
        Ok(Resolved {
            dirent: target,
            path,
            redirected_from,
        })
    }

    /// The search mode that `select` maps to for this archive, if any.
    pub fn mode_for(&self, select: crate::search::ModeSelect) -> Option<Mode> {
        use crate::search::ModeSelect;
        match select {
            ModeSelect::Auto => self.info.search_mode,
            ModeSelect::Fulltext => self.has_fulltext().then_some(Mode::Fulltext),
            ModeSelect::Title => self.has_title_index().then_some(Mode::Title),
            ModeSelect::Listing => self.has_listing().then_some(Mode::Listing),
        }
    }
}

fn strip_uri(path: &str) -> &str {
    match path.strip_prefix("zim://") {
        Some(rest) => rest.split_once('/').map_or("", |(_, p)| p),
        None => path,
    }
}

/// A resolved entry: the item itself (redirects followed) and its canonical user path.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub dirent: Arc<Dirent>,
    pub path: String,
    pub redirected_from: Option<String>,
}

struct CacheInner {
    map: LruCache<(usize, u32, bool), Arc<Document>>,
    bytes: usize,
    hits: u64,
    misses: u64,
}

/// LRU of extracted documents, bounded by bytes of Markdown + text.
struct ExtractCache {
    inner: Mutex<CacheInner>,
    budget: usize,
}

fn document_bytes(doc: &Document) -> usize {
    doc.markdown.len() + doc.text.len() + doc.links.len() * 64 + doc.sections.len() * 48 + 256
}

impl ExtractCache {
    fn new(budget: usize) -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                map: LruCache::unbounded(),
                bytes: 0,
                hits: 0,
                misses: 0,
            }),
            budget,
        }
    }

    fn get(&self, key: (usize, u32, bool)) -> Option<Arc<Document>> {
        let mut g = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(d) = g.map.get(&key).cloned() {
            g.hits += 1;
            Some(d)
        } else {
            g.misses += 1;
            None
        }
    }

    fn insert(&self, key: (usize, u32, bool), doc: Arc<Document>) {
        let size = document_bytes(&doc);
        if size > self.budget {
            return;
        }
        let mut g = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(old) = g.map.put(key, doc) {
            g.bytes = g.bytes.saturating_sub(document_bytes(&old));
        }
        g.bytes += size;
        while g.bytes > self.budget {
            match g.map.pop_lru() {
                Some((_, old)) => g.bytes = g.bytes.saturating_sub(document_bytes(&old)),
                None => break,
            }
        }
    }

    fn stats(&self) -> ExtractCacheStats {
        let g = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ExtractCacheStats {
            entries: g.map.len(),
            bytes: g.bytes,
            budget_bytes: self.budget,
            hits: g.hits,
            misses: g.misses,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExtractCacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub budget_bytes: usize,
    pub hits: u64,
    pub misses: u64,
}

/// All archives of a library, opened and catalogued.
pub struct Library {
    slots: Vec<Slot>,
    by_name: HashMap<String, usize>,
    failures: Vec<ScanFailure>,
    extract_cache: ExtractCache,
    config: LibraryConfig,
    scan_time: Duration,
}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("archives", &self.slots.len())
            .field("failures", &self.failures.len())
            .finish_non_exhaustive()
    }
}

/// `*.zim` files plus the first part (`*.zimaa`) of split archives, sorted.
pub fn discover_files(dirs: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for dir in dirs {
        walk(dir, recursive, &mut out)?;
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn is_archive_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    std::path::Path::new(&name)
        .extension()
        .is_some_and(|ext| ext == "zim" || ext == "zimaa")
}

fn walk(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::Invalid(format!("cannot read directory {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let ty = entry.file_type()?;
        let is_dir = if ty.is_symlink() {
            path.is_dir()
        } else {
            ty.is_dir()
        };
        if is_dir {
            if recursive {
                walk(&path, recursive, out)?;
            }
        } else if is_archive_file(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn glob_ci(pattern: &str, value: &str) -> bool {
    glob_match::glob_match(&pattern.to_lowercase(), &value.to_lowercase())
}

impl Library {
    /// Scan the configured directories and open every archive. Archives that fail to
    /// open are recorded in [`Library::failures`] rather than aborting the scan.
    pub fn scan(config: LibraryConfig) -> Result<Self> {
        let t0 = Instant::now();
        let mut files = discover_files(&config.dirs, config.recursive)?;
        files.extend(config.files.iter().cloned());
        files.dedup();
        let per_archive = if files.is_empty() {
            config.cluster_cache_bytes
        } else {
            (config.cluster_cache_bytes / files.len()).clamp(4 << 20, 256 << 20)
        };
        let opened: Vec<(PathBuf, Result<Slot>)> = files
            .par_iter()
            .map(|f| (f.clone(), Slot::open(f, per_archive)))
            .collect();
        let mut slots = Vec::with_capacity(opened.len());
        let mut failures = Vec::new();
        let mut taken: HashSet<String> = HashSet::new();
        for (file, result) in opened {
            match result {
                Ok(mut slot) => {
                    let name = unique_name(&slot, &file, &mut taken);
                    slot.info.name = name;
                    slot.info.priority = priority_for(&config.priorities, &slot.info.name, &file);
                    slots.push(slot);
                }
                Err(e) => failures.push(ScanFailure {
                    file: file.display().to_string(),
                    error: e.to_string(),
                }),
            }
        }
        let by_name = slots
            .iter()
            .enumerate()
            .map(|(i, s)| (s.info.name.clone(), i))
            .collect();
        Ok(Self {
            slots,
            by_name,
            failures,
            extract_cache: ExtractCache::new(config.extract_cache_bytes),
            config,
            scan_time: t0.elapsed(),
        })
    }

    pub fn config(&self) -> &LibraryConfig {
        &self.config
    }

    pub fn scan_time(&self) -> Duration {
        self.scan_time
    }

    pub fn failures(&self) -> &[ScanFailure] {
        &self.failures
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn archives(&self) -> impl Iterator<Item = &ArchiveInfo> {
        self.slots.iter().map(|s| &s.info)
    }

    /// Catalogue entries matching `filter` (substring over name, title, description,
    /// language, category, tags), all of them when `filter` is `None`.
    pub fn list(&self, filter: Option<&str>) -> Vec<&ArchiveInfo> {
        self.archives()
            .filter(|a| filter.is_none_or(|f| a.matches_filter(f)))
            .collect()
    }

    pub fn slot_index(&self, name: &str) -> Option<usize> {
        let name = name.trim();
        if let Some(&i) = self.by_name.get(name) {
            return Some(i);
        }
        self.slots.iter().position(|s| {
            s.info.name.eq_ignore_ascii_case(name)
                || s.info.uuid == name
                || file_stem_name(Path::new(&s.info.file)).eq_ignore_ascii_case(name)
        })
    }

    pub fn slot(&self, name: &str) -> Option<&Slot> {
        self.slot_index(name).map(|i| &self.slots[i])
    }

    /// Look up one archive by name (or uuid, or file stem).
    pub fn get(&self, name: &str) -> Result<&Slot> {
        self.slot(name)
            .ok_or_else(|| Error::NoSuchArchive(name.to_string()))
    }

    /// Indexes of the archives selected by `patterns` (names, uuids, file stems or
    /// globs such as `devdocs_*`; comma-separated lists are accepted too). An empty
    /// list selects every archive. Unknown patterns are an error.
    pub fn select(&self, patterns: &[String]) -> Result<Vec<usize>> {
        if self.slots.is_empty() {
            return Err(Error::Empty);
        }
        let pats: Vec<&str> = patterns
            .iter()
            .flat_map(|p| p.split(','))
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        if pats.is_empty() {
            return Ok((0..self.slots.len()).collect());
        }
        let mut picked: Vec<usize> = Vec::new();
        for pat in pats {
            let mut matched = false;
            if let Some(i) = self.slot_index(pat) {
                picked.push(i);
                matched = true;
            } else {
                for (i, s) in self.slots.iter().enumerate() {
                    if glob_ci(pat, &s.info.name)
                        || glob_ci(pat, &file_stem_name(Path::new(&s.info.file)))
                    {
                        picked.push(i);
                        matched = true;
                    }
                }
            }
            if !matched {
                return Err(Error::NoSuchArchive(pat.to_string()));
            }
        }
        picked.sort_unstable();
        picked.dedup();
        Ok(picked)
    }

    /// Extract an entry (cached). Redirects must already be resolved.
    pub fn document(&self, slot_idx: usize, dirent: &Dirent) -> Result<Arc<Document>> {
        self.document_inner(slot_idx, dirent, None)
    }

    /// Like [`Library::document`], but very large items are extracted from their
    /// first `max_snippet_source_bytes` only: enough for a snippet, cheap enough for a
    /// search page.
    pub fn document_for_snippet(&self, slot_idx: usize, dirent: &Dirent) -> Result<Arc<Document>> {
        let cap = self.config.max_snippet_source_bytes;
        let slot = &self.slots[slot_idx];
        let size = slot.archive.item_size(dirent)?;
        if cap > 0 && size > cap as u64 {
            self.document_inner(slot_idx, dirent, Some(cap))
        } else {
            self.document_inner(slot_idx, dirent, None)
        }
    }

    fn document_inner(
        &self,
        slot_idx: usize,
        dirent: &Dirent,
        cap: Option<usize>,
    ) -> Result<Arc<Document>> {
        let key = (slot_idx, dirent.index, cap.is_some());
        if let Some(doc) = self.extract_cache.get(key) {
            return Ok(doc);
        }
        let slot = &self.slots[slot_idx];
        let prefix = slot.link_prefix();
        let doc = match cap {
            None => zimz_extract::extract(&slot.archive, dirent, slot.adapter, Some(&prefix))?,
            Some(cap) => {
                let data = slot.archive.item_data(dirent)?;
                let mut end = cap.min(data.len());
                // back up to a UTF-8 boundary (a continuation byte is 0b10xxxxxx)
                while end > 0 && end < data.len() && (data[end] & 0xC0) == 0x80 {
                    end -= 1;
                }
                let html = String::from_utf8_lossy(&data[..end]);
                let opts = zimz_extract::RenderOptions {
                    base_namespace: dirent.namespace,
                    base_path: &dirent.path,
                    new_scheme: slot.archive.uses_new_namespace_scheme(),
                    link_prefix: Some(&prefix),
                    image_sources: false,
                };
                zimz_extract::extract_html(&html, slot.adapter, Some(dirent.title()), &opts)
            }
        };
        let doc = Arc::new(doc);
        self.extract_cache.insert(key, doc.clone());
        Ok(doc)
    }

    pub fn extract_cache_stats(&self) -> ExtractCacheStats {
        self.extract_cache.stats()
    }
}

fn unique_name(slot: &Slot, file: &Path, taken: &mut HashSet<String>) -> String {
    let stem = file_stem_name(file);
    let mut candidates = Vec::new();
    if let Some(n) = ArchiveInfo::metadata_name(&slot.archive) {
        candidates.push(n);
    }
    candidates.push(stem.clone());
    for c in &candidates {
        if taken.insert(c.clone()) {
            return c.clone();
        }
    }
    let base = candidates.last().cloned().unwrap_or(stem);
    let mut n = 2;
    loop {
        let c = format!("{base}~{n}");
        if taken.insert(c.clone()) {
            return c;
        }
        n += 1;
    }
}

fn priority_for(priorities: &[(String, f64)], name: &str, file: &Path) -> f64 {
    let stem = file_stem_name(file);
    priorities
        .iter()
        .find(|(pat, _)| glob_ci(pat, name) || glob_ci(pat, &stem))
        .map_or(1.0, |(_, w)| *w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_stripping() {
        assert_eq!(strip_uri("zim://wiki/Foo/bar"), "Foo/bar");
        assert_eq!(strip_uri("zim://wiki"), "");
        assert_eq!(strip_uri("Foo"), "Foo");
    }

    #[test]
    fn glob_is_case_insensitive() {
        assert!(glob_ci("DevDocs_*", "devdocs_en_git_2026-04"));
        assert!(!glob_ci("wiki*", "devdocs"));
    }

    #[test]
    fn priorities_first_match_wins() {
        let p = vec![("wikipedia*".to_string(), 2.0), ("*".to_string(), 0.5)];
        assert!((priority_for(&p, "wikipedia_en", Path::new("x.zim")) - 2.0).abs() < 1e-9);
        assert!((priority_for(&p, "devdocs", Path::new("x.zim")) - 0.5).abs() < 1e-9);
        assert!((priority_for(&[], "devdocs", Path::new("x.zim")) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn extract_cache_evicts_by_bytes() {
        let cache = ExtractCache::new(2000);
        let make = |n: usize| {
            Arc::new(Document {
                title: String::new(),
                markdown: "x".repeat(n),
                text: String::new(),
                sections: Vec::new(),
                links: Vec::new(),
                word_count: 0,
                adapter: Adapter::Generic,
                source_path: String::new(),
            })
        };
        cache.insert((0, 1, false), make(500));
        cache.insert((0, 2, false), make(500));
        cache.insert((0, 3, false), make(500));
        let s = cache.stats();
        assert!(s.bytes <= 2000, "{s:?}");
        assert!(cache.get((0, 3, false)).is_some());
        assert!(
            cache.get((0, 1, false)).is_none(),
            "oldest entry should be evicted"
        );
        assert!(document_bytes(&make(5000)) > 2000);
        cache.insert((0, 4, false), make(5000));
        assert!(
            cache.get((0, 4, false)).is_none(),
            "oversize documents are not cached"
        );
    }
}
