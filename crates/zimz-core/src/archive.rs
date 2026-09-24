// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! [`Archive`]: an opened ZIM file.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;
use std::ops::{Deref, Range};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use md5::{Digest, Md5};

use crate::cache::{ClusterCache, DirentCache};
use crate::cluster::{ClusterData, ClusterReadConfig, Compression, read_cluster};
use crate::dirent::{Dirent, DirentKind, REDIRECT_HOP_LIMIT, split_long_path};
use crate::header::{HEADER_SIZE, Header};
use crate::metadata::{IllustrationInfo, parse_counter};
use crate::mime::MimeList;
use crate::source::{FileRange, MmapSource, MultiPartSource, Source, SubSource};
use crate::title::TitleIndex;
use crate::{Error, Result};

/// Tunables for [`Archive::open_with`].
#[derive(Debug, Clone)]
pub struct OpenConfig {
    /// Byte budget for decoded clusters kept in memory.
    pub cluster_cache_bytes: usize,
    /// Number of parsed directory entries kept in memory.
    pub dirent_cache_entries: usize,
    /// Refuse to decompress a single cluster beyond this size.
    pub max_cluster_bytes: u64,
}

impl Default for OpenConfig {
    fn default() -> Self {
        Self {
            cluster_cache_bytes: 256 << 20,
            dirent_cache_entries: 4096,
            max_cluster_bytes: 2 << 30,
        }
    }
}

/// Absolute byte range of an item stored in an uncompressed cluster (what libzim calls
/// "direct access information"). This is how the embedded Xapian indexes are located.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectAccess {
    pub offset: u64,
    pub len: u64,
}

/// The bytes of one item.
pub struct Blob<'a> {
    inner: BlobInner<'a>,
}

enum BlobInner<'a> {
    Borrowed(Cow<'a, [u8]>),
    Cached {
        cluster: Arc<ClusterData>,
        start: usize,
        end: usize,
    },
}

impl Deref for Blob<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match &self.inner {
            BlobInner::Borrowed(c) => c,
            BlobInner::Cached {
                cluster,
                start,
                end,
            } => &cluster
                .owned_bytes()
                .expect("cached blobs come from decompressed payloads")[*start..*end],
        }
    }
}

impl AsRef<[u8]> for Blob<'_> {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl fmt::Debug for Blob<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Blob({} bytes)", self.len())
    }
}

impl Blob<'_> {
    pub fn into_vec(self) -> Vec<u8> {
        match self.inner {
            BlobInner::Borrowed(c) => c.into_owned(),
            BlobInner::Cached { .. } => self.to_vec(),
        }
    }
}

/// An opened ZIM archive. Cheap to share behind an `Arc`; all methods take `&self`.
pub struct Archive {
    source: Box<dyn Source>,
    header: Header,
    mimes: MimeList,
    title_index: TitleIndex,
    dirents: DirentCache,
    clusters: ClusterCache,
    config: OpenConfig,
}

impl fmt::Debug for Archive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Archive")
            .field("source", &self.source.describe())
            .field(
                "version",
                &format!("{}.{}", self.header.major, self.header.minor),
            )
            .field("entries", &self.header.entry_count)
            .field("clusters", &self.header.cluster_count)
            .finish_non_exhaustive()
    }
}

fn is_split_part_name(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
        n.len() > 6
            && Path::new(&n[..n.len() - 2])
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("zim"))
            && n[n.len() - 2..].bytes().all(|b| b.is_ascii_lowercase())
    })
}

impl Archive {
    /// Open a `.zim` file, or a split archive given its first part (`foo.zimaa`) or its
    /// base name (`foo.zim` when only `foo.zimaa`, `foo.zimab`, … exist).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, OpenConfig::default())
    }

    pub fn open_with(path: impl AsRef<Path>, config: OpenConfig) -> Result<Self> {
        let path = path.as_ref();
        if is_split_part_name(path) {
            let parts = MultiPartSource::discover(path)?;
            return Self::from_source_with(Box::new(MultiPartSource::open(parts)?), config);
        }
        if !path.exists() {
            let mut first = path.as_os_str().to_owned();
            first.push("aa");
            let first = PathBuf::from(first);
            if first.is_file() {
                let parts = MultiPartSource::discover(&first)?;
                return Self::from_source_with(Box::new(MultiPartSource::open(parts)?), config);
            }
        }
        Self::from_source_with(Box::new(MmapSource::open(path)?), config)
    }

    /// Open a ZIM stored at `offset` inside a bigger file.
    pub fn open_embedded(
        path: impl AsRef<Path>,
        offset: u64,
        len: u64,
        config: OpenConfig,
    ) -> Result<Self> {
        let source = SubSource::new(MmapSource::open(path)?, offset, len)?;
        Self::from_source_with(Box::new(source), config)
    }

    /// Open a ZIM stored as consecutive byte ranges of one or more files (libzim's
    /// `FdInput` list).
    pub fn open_ranges(
        ranges: impl IntoIterator<Item = FileRange>,
        config: OpenConfig,
    ) -> Result<Self> {
        Self::from_source_with(Box::new(MultiPartSource::open_ranges(ranges)?), config)
    }

    pub fn from_source(source: Box<dyn Source>) -> Result<Self> {
        Self::from_source_with(source, OpenConfig::default())
    }

    pub fn from_source_with(source: Box<dyn Source>, config: OpenConfig) -> Result<Self> {
        let size = source.len();
        let head = source.slice(0, HEADER_SIZE.min(size as usize))?;
        let header = Header::parse(&head)?;
        header.validate(size)?;

        // The MIME list ends at the first of: the pointer lists, the first dirent, the
        // first cluster (mirrors libzim). Cap the window so a missing terminator is caught.
        let read_u64 = |off: u64| -> Result<u64> {
            let s = source.slice(off, 8)?;
            Ok(u64::from_le_bytes(s[..8].try_into().expect("8 bytes")))
        };
        let mut bound = header.path_ptr_pos.min(header.cluster_ptr_pos);
        if header.has_title_pointer_list() {
            bound = bound.min(header.title_ptr_pos);
        }
        if header.entry_count > 0 {
            bound = bound
                .min(read_u64(header.path_ptr_pos)?)
                .min(read_u64(header.cluster_ptr_pos)?);
        }
        let window = bound
            .saturating_sub(header.mime_list_pos)
            .min(size - header.mime_list_pos)
            .min(64 * 1024) as usize;
        let mimes = MimeList::parse(&source.slice(header.mime_list_pos, window)?)?;

        let mut archive = Self {
            source,
            header,
            mimes,
            title_index: TitleIndex::None,
            dirents: DirentCache::new(config.dirent_cache_entries),
            clusters: ClusterCache::new(config.cluster_cache_bytes),
            config,
        };
        archive.title_index = archive.resolve_title_index()?;
        Ok(archive)
    }

    /// libzim's order: `X/listing/titleOrdered/v1` in an uncompressed cluster, else the
    /// header's title pointer list, else none.
    fn resolve_title_index(&self) -> Result<TitleIndex> {
        if let Some(d) = self.entry_by_path(b'X', "listing/titleOrdered/v1")?
            && d.is_item()
            && let Some(da) = self.direct_access(&d)?
        {
            return Ok(TitleIndex::FrontArticles {
                offset: da.offset,
                count: (da.len / 4) as u32,
            });
        }
        if self.header.has_title_pointer_list() {
            return Ok(TitleIndex::Header {
                offset: self.header.title_ptr_pos,
                count: self.header.entry_count,
            });
        }
        Ok(TitleIndex::None)
    }

    // ----- basic accessors -------------------------------------------------------

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn source(&self) -> &dyn Source {
        self.source.as_ref()
    }

    pub fn size(&self) -> u64 {
        self.source.len()
    }

    pub fn describe(&self) -> String {
        self.source.describe()
    }

    pub fn uuid(&self) -> String {
        self.header.uuid_string()
    }

    pub fn entry_count(&self) -> u32 {
        self.header.entry_count
    }

    pub fn cluster_count(&self) -> u32 {
        self.header.cluster_count
    }

    pub fn mime_list(&self) -> &MimeList {
        &self.mimes
    }

    /// MIME type of an item (`None` for redirects and placeholders, or when the index
    /// is out of range).
    pub fn mime_type(&self, dirent: &Dirent) -> Option<&str> {
        if MimeList::is_special(dirent.mime) {
            None
        } else {
            self.mimes.get(dirent.mime)
        }
    }

    pub fn uses_new_namespace_scheme(&self) -> bool {
        self.header.uses_new_namespace_scheme()
    }

    /// The path libzim reports for an entry: bare for new-scheme archives, prefixed with
    /// `<ns>/` for old ones.
    pub fn user_path(&self, dirent: &Dirent) -> String {
        if self.uses_new_namespace_scheme() {
            dirent.path.clone()
        } else {
            dirent.full_path()
        }
    }

    /// `b'C'` for the new scheme, `b'A'` for old archives.
    pub fn content_namespace(&self) -> u8 {
        if self.uses_new_namespace_scheme() {
            b'C'
        } else {
            b'A'
        }
    }

    pub fn title_index(&self) -> TitleIndex {
        self.title_index
    }

    pub fn config(&self) -> &OpenConfig {
        &self.config
    }

    /// `(cached clusters, bytes)`.
    pub fn cluster_cache_stats(&self) -> (usize, usize) {
        self.clusters.stats()
    }

    fn read_u64(&self, offset: u64) -> Result<u64> {
        let s = self.source.slice(offset, 8)?;
        Ok(u64::from_le_bytes(s[..8].try_into().expect("8 bytes")))
    }

    fn read_u32(&self, offset: u64) -> Result<u32> {
        let s = self.source.slice(offset, 4)?;
        Ok(u32::from_le_bytes(s[..4].try_into().expect("4 bytes")))
    }

    // ----- directory entries -----------------------------------------------------

    /// File offset of a dirent, validated against the archive bounds.
    pub fn dirent_offset(&self, index: u32) -> Result<u64> {
        if index >= self.header.entry_count {
            return Err(Error::corrupt(format!(
                "entry index {index} is out of range"
            )));
        }
        let off = self.read_u64(self.header.path_ptr_pos + 8 * u64::from(index))?;
        if off < self.header.mime_list_pos || off.saturating_add(12) > self.source.len() {
            return Err(Error::corrupt(format!(
                "dirent pointer {index} is out of bounds"
            )));
        }
        Ok(off)
    }

    /// The entry at `index` in path order (cached).
    pub fn entry(&self, index: u32) -> Result<Arc<Dirent>> {
        if let Some(d) = self.dirents.get(index) {
            return Ok(d);
        }
        let offset = self.dirent_offset(index)?;
        let d = Arc::new(self.read_dirent(index, offset)?);
        self.dirents.insert(index, d.clone());
        Ok(d)
    }

    fn read_dirent(&self, index: u32, offset: u64) -> Result<Dirent> {
        let avail = self.source.len() - offset;
        for want in [2048u64, 64 * 1024, 132 * 1024] {
            let len = want.min(avail) as usize;
            let bytes = self.source.slice(offset, len)?;
            if let Some((d, _)) = Dirent::parse(index, &bytes)? {
                return Ok(d);
            }
            if len as u64 == avail {
                break;
            }
        }
        Err(Error::corrupt(format!(
            "dirent {index} is truncated or unterminated"
        )))
    }

    /// Binary search in path order. Returns `(found, index)`; when not found, `index`
    /// is the insertion point (first entry greater than the key).
    pub fn find_path(&self, namespace: u8, path: &str) -> Result<(bool, u32)> {
        let key = (namespace, path.as_bytes());
        let (mut lo, mut hi) = (0u32, self.header.entry_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let d = self.entry(mid)?;
            match (d.namespace, d.path.as_bytes()).cmp(&key) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => return Ok((true, mid)),
            }
        }
        Ok((false, lo))
    }

    pub fn entry_by_path(&self, namespace: u8, path: &str) -> Result<Option<Arc<Dirent>>> {
        let (found, idx) = self.find_path(namespace, path)?;
        if found {
            Ok(Some(self.entry(idx)?))
        } else {
            Ok(None)
        }
    }

    /// Look up `"C/foo"` style paths.
    pub fn entry_by_long_path(&self, long_path: &str) -> Result<Option<Arc<Dirent>>> {
        match split_long_path(long_path) {
            Some((ns, rest)) => self.entry_by_path(ns, rest),
            None => Ok(None),
        }
    }

    /// libzim-compatible resolution of a user-supplied path: new scheme → `C/<path>`,
    /// retrying without a leading `<ns>/`; old scheme → the namespace in the path, else
    /// `A`, `I`, `J`, `-` in turn.
    pub fn entry_by_path_compat(&self, path: &str) -> Result<Option<Arc<Dirent>>> {
        let stripped = path.strip_prefix('/').unwrap_or(path);
        if self.uses_new_namespace_scheme() {
            if let Some(d) = self.entry_by_path(b'C', stripped)? {
                return Ok(Some(d));
            }
            return match split_long_path(stripped) {
                Some((_, rest)) => self.entry_by_path(b'C', rest),
                None => Ok(None),
            };
        }
        let (explicit, rest) = match split_long_path(stripped) {
            Some((ns, rest)) => (Some(ns), rest),
            None => (None, stripped),
        };
        if let Some(ns) = explicit
            && let Some(d) = self.entry_by_path(ns, rest)?
        {
            return Ok(Some(d));
        }
        for ns in *b"AIJ-" {
            if let Some(d) = self.entry_by_path(ns, rest)? {
                return Ok(Some(d));
            }
        }
        Ok(None)
    }

    /// Index range of all entries in a namespace.
    pub fn namespace_range(&self, namespace: u8) -> Result<Range<u32>> {
        let start = self.find_path(namespace, "")?.1;
        let end = if namespace == u8::MAX {
            self.header.entry_count
        } else {
            self.find_path(namespace + 1, "")?.1
        };
        Ok(start..end)
    }

    /// Namespaces present, in order.
    pub fn namespaces(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut idx = 0;
        while idx < self.header.entry_count {
            let ns = self.entry(idx)?.namespace;
            out.push(ns);
            idx = self.namespace_range(ns)?.end.max(idx + 1);
        }
        Ok(out)
    }

    /// All entries in path order.
    pub fn entries(&self) -> impl Iterator<Item = Result<Arc<Dirent>>> + '_ {
        self.entries_in(0..self.header.entry_count)
    }

    pub fn entries_in(&self, range: Range<u32>) -> impl Iterator<Item = Result<Arc<Dirent>>> + '_ {
        range.map(move |i| self.entry(i))
    }

    /// Follow redirects (at most [`REDIRECT_HOP_LIMIT`] hops).
    pub fn resolve(&self, dirent: &Arc<Dirent>) -> Result<Arc<Dirent>> {
        let mut cur = dirent.clone();
        for _ in 0..REDIRECT_HOP_LIMIT {
            match cur.kind {
                DirentKind::Redirect { target } => cur = self.entry(target)?,
                _ => return Ok(cur),
            }
        }
        Err(Error::RedirectLoop(REDIRECT_HOP_LIMIT))
    }

    // ----- clusters and blobs ----------------------------------------------------

    pub fn cluster_offset(&self, index: u32) -> Result<u64> {
        if index >= self.header.cluster_count {
            return Err(Error::corrupt(format!(
                "cluster index {index} is out of range"
            )));
        }
        let off = self.read_u64(self.header.cluster_ptr_pos + 8 * u64::from(index))?;
        if off < self.header.mime_list_pos || off >= self.source.len() {
            return Err(Error::corrupt(format!(
                "cluster pointer {index} is out of bounds"
            )));
        }
        Ok(off)
    }

    /// Compression of a cluster, read from its info byte without decoding it.
    pub fn cluster_compression(&self, index: u32) -> Result<Compression> {
        let offset = self.cluster_offset(index)?;
        Compression::from_info_byte(self.source.slice(offset, 1)?[0])
    }

    /// The decoded cluster (cached).
    pub fn cluster(&self, index: u32) -> Result<Arc<ClusterData>> {
        if let Some(c) = self.clusters.get(index) {
            return Ok(c);
        }
        let offset = self.cluster_offset(index)?;
        let cfg = ClusterReadConfig {
            max_decompressed: self.config.max_cluster_bytes,
            max_blobs: u64::from(self.header.entry_count),
        };
        let data = Arc::new(read_cluster(self.source.as_ref(), index, offset, &cfg)?);
        self.clusters.insert(index, data.clone());
        Ok(data)
    }

    pub fn blob(&self, cluster: u32, blob: u32) -> Result<Blob<'_>> {
        let data = self.cluster(cluster)?;
        let (start, end) = data.blob_range(blob).ok_or_else(|| {
            Error::corrupt(format!("blob {blob} does not exist in cluster {cluster}"))
        })?;
        if let Some(base) = data.direct_base() {
            let bytes = self.source.slice(base + start, (end - start) as usize)?;
            return Ok(Blob {
                inner: BlobInner::Borrowed(bytes),
            });
        }
        Ok(Blob {
            inner: BlobInner::Cached {
                cluster: data,
                start: start as usize,
                end: end as usize,
            },
        })
    }

    /// Content of an item entry (redirects are an error; call [`Archive::resolve`] first).
    pub fn item_data(&self, dirent: &Dirent) -> Result<Blob<'_>> {
        let (c, b) = dirent.location_or_err()?;
        self.blob(c, b)
    }

    pub fn item_size(&self, dirent: &Dirent) -> Result<u64> {
        let (c, b) = dirent.location_or_err()?;
        let data = self.cluster(c)?;
        data.blob_size(b)
            .ok_or_else(|| Error::corrupt(format!("blob {b} does not exist in cluster {c}")))
    }

    /// Byte range of an item when it lives in an uncompressed cluster.
    pub fn direct_access(&self, dirent: &Dirent) -> Result<Option<DirectAccess>> {
        let (c, b) = dirent.location_or_err()?;
        if self.cluster_compression(c)?.is_compressed() {
            return Ok(None);
        }
        let data = self.cluster(c)?;
        let Some(base) = data.direct_base() else {
            return Ok(None);
        };
        let (start, end) = data
            .blob_range(b)
            .ok_or_else(|| Error::corrupt(format!("blob {b} does not exist in cluster {c}")))?;
        Ok(Some(DirectAccess {
            offset: base + start,
            len: end - start,
        }))
    }

    // ----- metadata and well-known entries --------------------------------------

    pub fn metadata(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let Some(d) = self.entry_by_path(b'M', name)? else {
            return Ok(None);
        };
        let d = self.resolve(&d)?;
        if !d.is_item() {
            return Ok(None);
        }
        Ok(Some(self.item_data(&d)?.into_vec()))
    }

    pub fn metadata_string(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .metadata(name)?
            .map(|v| String::from_utf8_lossy(&v).into_owned()))
    }

    pub fn metadata_keys(&self) -> Result<Vec<String>> {
        self.entries_in(self.namespace_range(b'M')?)
            .map(|d| d.map(|d| d.path.clone()))
            .collect()
    }

    /// Parsed `M/Counter`.
    pub fn counter(&self) -> Result<Vec<(String, u64)>> {
        Ok(self
            .metadata_string("Counter")?
            .map(|s| parse_counter(&s))
            .unwrap_or_default())
    }

    pub fn illustration_sizes(&self) -> Result<Vec<IllustrationInfo>> {
        Ok(self
            .metadata_keys()?
            .iter()
            .filter_map(|k| IllustrationInfo::parse(k))
            .collect())
    }

    /// `M/Illustration_<w>x<h>@<scale>`, falling back to the old-scheme favicon entries.
    pub fn illustration(&self, width: u32, height: u32, scale: f32) -> Result<Option<Vec<u8>>> {
        let info = IllustrationInfo {
            width,
            height,
            scale,
        };
        if let Some(png) = self.metadata(&info.metadata_name())? {
            return Ok(Some(png));
        }
        if width == 48 && height == 48 && !self.uses_new_namespace_scheme() {
            for (ns, path) in [
                (b'-', "favicon"),
                (b'-', "favicon.png"),
                (b'I', "favicon"),
                (b'I', "favicon.png"),
            ] {
                if let Some(d) = self.entry_by_path(ns, path)? {
                    let d = self.resolve(&d)?;
                    if d.is_item() {
                        return Ok(Some(self.item_data(&d)?.into_vec()));
                    }
                }
            }
        }
        Ok(None)
    }

    /// The main page entry (`W/mainPage`, usually a redirect, else the header's index).
    pub fn main_entry(&self) -> Result<Option<Arc<Dirent>>> {
        if let Some(d) = self.entry_by_path(b'W', "mainPage")? {
            return Ok(Some(d));
        }
        if self.header.has_main_page() {
            return Ok(Some(self.entry(self.header.main_page)?));
        }
        Ok(None)
    }

    // ----- title index -----------------------------------------------------------

    /// Entry index at `pos` of the title-ordered list.
    pub fn title_entry_index(&self, pos: u32) -> Result<u32> {
        let Some(offset) = self.title_index.offset() else {
            return Err(Error::NoTitleIndex);
        };
        if pos >= self.title_index.len() {
            return Err(Error::corrupt(format!(
                "title position {pos} is out of range"
            )));
        }
        let idx = self.read_u32(offset + 4 * u64::from(pos))?;
        if idx >= self.header.entry_count {
            return Err(Error::corrupt(format!(
                "title index position {pos} points outside the entry table"
            )));
        }
        Ok(idx)
    }

    pub fn entry_by_title_position(&self, pos: u32) -> Result<Arc<Dirent>> {
        self.entry(self.title_entry_index(pos)?)
    }

    /// First position in the title-ordered list whose `(namespace, title)` is `>=` the key.
    pub fn title_lower_bound(&self, namespace: u8, title: &[u8]) -> Result<u32> {
        if self.title_index == TitleIndex::None {
            return Err(Error::NoTitleIndex);
        }
        let key = (namespace, title);
        let (mut lo, mut hi) = (0u32, self.title_index.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let d = self.entry_by_title_position(mid)?;
            if (d.namespace, d.title().as_bytes()) < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    /// Positions in the title-ordered list whose title starts with `prefix`
    /// (byte-wise, case-sensitive, like libzim's `findByTitle`).
    pub fn find_title_prefix(&self, namespace: u8, prefix: &str) -> Result<Range<u32>> {
        let start = self.title_lower_bound(namespace, prefix.as_bytes())?;
        let mut upper = prefix.as_bytes().to_vec();
        while upper.last() == Some(&0xff) {
            upper.pop();
        }
        let end = match upper.last_mut() {
            Some(last) => {
                *last += 1;
                self.title_lower_bound(namespace, &upper)?
            }
            None if namespace == u8::MAX => self.title_index.len(),
            None => self.title_lower_bound(namespace + 1, b"")?,
        };
        Ok(start..end.max(start))
    }

    /// libzim's `getEntryByTitle`: exact title match in the content namespace (new
    /// scheme), or in `A`, `I`, `J`, `-` in turn (old scheme).
    pub fn entry_by_title(&self, title: &str) -> Result<Option<Arc<Dirent>>> {
        let namespaces: &[u8] = if self.uses_new_namespace_scheme() {
            b"C"
        } else {
            b"AIJ-"
        };
        for &ns in namespaces {
            let range = self.find_title_prefix(ns, title)?;
            for pos in range {
                let d = self.entry_by_title_position(pos)?;
                if d.title() == title {
                    return Ok(Some(d));
                }
            }
        }
        Ok(None)
    }

    /// Entries in title order (front articles only when the v1 listing is used).
    pub fn title_ordered(&self) -> impl Iterator<Item = Result<Arc<Dirent>>> + '_ {
        (0..self.title_index.len()).map(move |p| self.entry_by_title_position(p))
    }

    /// libzim's `getArticleCount`: the front-article listing size, else the `text/html`
    /// total from `M/Counter`, else the size of the content namespace.
    pub fn article_count(&self) -> Result<u32> {
        if let TitleIndex::FrontArticles { count, .. } = self.title_index {
            return Ok(count);
        }
        let html: u64 = self
            .counter()?
            .iter()
            .filter(|(m, _)| m.starts_with("text/html"))
            .map(|(_, n)| *n)
            .sum();
        if html > 0 {
            return Ok(html.min(u64::from(u32::MAX)) as u32);
        }
        Ok(self.namespace_range(self.content_namespace())?.len() as u32)
    }

    // ----- embedded search indexes ----------------------------------------------

    fn index_access(&self, candidates: &[(u8, &str)]) -> Result<Option<DirectAccess>> {
        for (ns, path) in candidates {
            if let Some(d) = self.entry_by_path(*ns, path)? {
                return if d.is_item() {
                    self.direct_access(&d)
                } else {
                    Ok(None)
                };
            }
        }
        Ok(None)
    }

    /// Byte range of the embedded Xapian full-text index, if present and directly
    /// readable.
    pub fn fulltext_index(&self) -> Result<Option<DirectAccess>> {
        self.index_access(&[(b'X', "fulltext/xapian"), (b'Z', "/fulltextIndex/xapian")])
    }

    /// Byte range of the embedded Xapian title (suggestion) index.
    pub fn title_xapian_index(&self) -> Result<Option<DirectAccess>> {
        self.index_access(&[(b'X', "title/xapian")])
    }

    pub fn has_fulltext_index(&self) -> bool {
        matches!(self.fulltext_index(), Ok(Some(_)))
    }

    pub fn has_title_xapian_index(&self) -> bool {
        matches!(self.title_xapian_index(), Ok(Some(_)))
    }

    // ----- checksum --------------------------------------------------------------

    pub fn stored_checksum(&self) -> Result<[u8; 16]> {
        if !self.header.has_checksum() {
            return Err(Error::NoChecksum);
        }
        let s = self.source.slice(self.header.checksum_pos, 16)?;
        Ok(s[..16].try_into().expect("16 bytes"))
    }

    pub fn compute_checksum(&self) -> Result<[u8; 16]> {
        if !self.header.has_checksum() {
            return Err(Error::NoChecksum);
        }
        let mut hasher = Md5::new();
        let mut pos = 0u64;
        let end = self.header.checksum_pos;
        while pos < end {
            let len = (end - pos).min(4 << 20) as usize;
            hasher.update(&*self.source.slice(pos, len)?);
            pos += len as u64;
        }
        Ok(hasher.finalize().into())
    }

    pub fn verify_checksum(&self) -> Result<bool> {
        Ok(self.stored_checksum()? == self.compute_checksum()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Archive>();
    }

    #[test]
    fn split_names() {
        assert!(is_split_part_name(Path::new("/x/foo.zimaa")));
        assert!(is_split_part_name(Path::new("foo.zimzz")));
        assert!(!is_split_part_name(Path::new("foo.zim")));
        assert!(!is_split_part_name(Path::new("foo.zimA1")));
    }
}
