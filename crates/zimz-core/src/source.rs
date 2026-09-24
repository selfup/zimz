// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Byte sources an archive can be read from: a memory-mapped file, a set of split
//! parts (`foo.zimaa`, `foo.zimab`, …), a byte range inside a bigger file (embedded
//! ZIM), or an in-memory buffer (tests and fuzzing).

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::Mmap;

use crate::{Error, Result};

/// Random-access, thread-safe byte source.
pub trait Source: Send + Sync {
    /// Total size in bytes.
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `len` bytes at `offset`, borrowed when they are contiguous in memory, copied
    /// otherwise (e.g. a range straddling two split parts).
    fn slice(&self, offset: u64, len: usize) -> Result<Cow<'_, [u8]>>;

    /// Everything from `offset` to the end, borrowed, when that range is contiguous in
    /// memory. `None` means the caller must stream with [`SourceReader`] instead.
    fn contiguous_tail(&self, offset: u64) -> Option<&[u8]>;

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let s = self.slice(offset, buf.len())?;
        buf.copy_from_slice(&s);
        Ok(())
    }

    /// Human-readable description (file path, part list, …) for diagnostics.
    fn describe(&self) -> String;
}

pub(crate) fn check_range(offset: u64, len: u64, size: u64) -> Result<()> {
    match offset.checked_add(len) {
        Some(end) if end <= size => Ok(()),
        _ => Err(Error::OutOfBounds { offset, len, size }),
    }
}

fn map_file(path: &Path) -> Result<Mmap> {
    let file = File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Err(Error::corrupt(format!("{} is empty", path.display())));
    }
    // SAFETY: the mapping is read-only and never written through. ZIM archives are
    // immutable artifacts; if another process truncates the file while it is mapped the
    // process may receive SIGBUS, which is the standard mmap caveat accepted here.
    let mmap = unsafe { Mmap::map(&file)? };
    Ok(mmap)
}

/// A single memory-mapped file.
#[derive(Debug)]
pub struct MmapSource {
    mmap: Mmap,
    path: PathBuf,
}

impl MmapSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mmap = map_file(&path)?;
        Ok(Self { mmap, path })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.mmap
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Source for MmapSource {
    fn len(&self) -> u64 {
        self.mmap.len() as u64
    }

    fn slice(&self, offset: u64, len: usize) -> Result<Cow<'_, [u8]>> {
        check_range(offset, len as u64, self.len())?;
        let start = offset as usize;
        Ok(Cow::Borrowed(&self.mmap[start..start + len]))
    }

    fn contiguous_tail(&self, offset: u64) -> Option<&[u8]> {
        (offset <= self.len()).then(|| &self.mmap[offset as usize..])
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// An in-memory buffer.
#[derive(Debug, Clone)]
pub struct MemorySource {
    data: Vec<u8>,
}

impl MemorySource {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }
}

impl Source for MemorySource {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn slice(&self, offset: u64, len: usize) -> Result<Cow<'_, [u8]>> {
        check_range(offset, len as u64, self.len())?;
        let start = offset as usize;
        Ok(Cow::Borrowed(&self.data[start..start + len]))
    }

    fn contiguous_tail(&self, offset: u64) -> Option<&[u8]> {
        (offset <= self.len()).then(|| &self.data[offset as usize..])
    }

    fn describe(&self) -> String {
        format!("<memory {} bytes>", self.data.len())
    }
}

/// A byte range inside another source (a ZIM embedded in a bigger file).
#[derive(Debug)]
pub struct SubSource<S> {
    inner: S,
    base: u64,
    len: u64,
}

impl<S: Source> SubSource<S> {
    pub fn new(inner: S, base: u64, len: u64) -> Result<Self> {
        check_range(base, len, inner.len())?;
        Ok(Self { inner, base, len })
    }
}

impl<S: Source> Source for SubSource<S> {
    fn len(&self) -> u64 {
        self.len
    }

    fn slice(&self, offset: u64, len: usize) -> Result<Cow<'_, [u8]>> {
        check_range(offset, len as u64, self.len)?;
        self.inner.slice(self.base + offset, len)
    }

    fn contiguous_tail(&self, offset: u64) -> Option<&[u8]> {
        if offset > self.len {
            return None;
        }
        let tail = self.inner.contiguous_tail(self.base + offset)?;
        Some(&tail[..(self.len - offset) as usize])
    }

    fn describe(&self) -> String {
        format!("{} [{}..+{}]", self.inner.describe(), self.base, self.len)
    }
}

#[derive(Debug)]
struct Part {
    /// Position of this part in the logical address space.
    start: u64,
    mmap: Arc<Mmap>,
    /// Byte range of the mapped file that this part covers.
    base: usize,
    len: usize,
    path: PathBuf,
}

impl Part {
    fn bytes(&self) -> &[u8] {
        &self.mmap[self.base..self.base + self.len]
    }
}

/// A byte range of a file, for [`MultiPartSource::open_ranges`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRange {
    pub path: PathBuf,
    pub offset: u64,
    pub len: u64,
}

/// An archive assembled from consecutive byte ranges: split `.zimaa` files, or pieces
/// of a container file (libzim's `FdInput` list).
#[derive(Debug)]
pub struct MultiPartSource {
    parts: Vec<Part>,
    len: u64,
}

impl MultiPartSource {
    /// Open whole files as consecutive parts, in order.
    pub fn open<P: AsRef<Path>>(paths: impl IntoIterator<Item = P>) -> Result<Self> {
        let mut ranges = Vec::new();
        for p in paths {
            let path = p.as_ref().to_path_buf();
            let len = std::fs::metadata(&path)?.len();
            ranges.push(FileRange {
                path,
                offset: 0,
                len,
            });
        }
        Self::open_ranges(ranges)
    }

    /// Open arbitrary byte ranges as consecutive parts. A file appearing several times is
    /// mapped once.
    pub fn open_ranges(ranges: impl IntoIterator<Item = FileRange>) -> Result<Self> {
        let mut maps: HashMap<PathBuf, Arc<Mmap>> = HashMap::new();
        let mut parts = Vec::new();
        let mut start = 0u64;
        for r in ranges {
            let mmap = if let Some(m) = maps.get(&r.path) {
                m.clone()
            } else {
                let m = Arc::new(map_file(&r.path)?);
                maps.insert(r.path.clone(), m.clone());
                m
            };
            check_range(r.offset, r.len, mmap.len() as u64)?;
            if r.len == 0 {
                continue;
            }
            parts.push(Part {
                start,
                mmap,
                base: r.offset as usize,
                len: r.len as usize,
                path: r.path,
            });
            start += r.len;
        }
        if parts.is_empty() {
            return Err(Error::corrupt("split archive has no parts"));
        }
        Ok(Self { parts, len: start })
    }

    /// Given the first part (`foo.zimaa`), list `foo.zimaa`, `foo.zimab`, … `foo.zimzz`
    /// as long as the files exist (parts must be consecutive).
    pub fn discover(first: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
        let first = first.as_ref();
        let name = first
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::corrupt("split archive name is not valid UTF-8"))?;
        let bytes = name.as_bytes();
        if bytes.len() < 3 || !bytes[bytes.len() - 2..].iter().all(u8::is_ascii_lowercase) {
            return Err(Error::corrupt(format!(
                "{name} does not look like a split archive part"
            )));
        }
        let stem = &name[..name.len() - 2];
        let dir = first.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut out = Vec::new();
        'outer: for a in b'a'..=b'z' {
            for b in b'a'..=b'z' {
                let candidate = dir.join(format!("{stem}{}{}", a as char, b as char));
                if candidate.is_file() {
                    out.push(candidate);
                } else {
                    break 'outer;
                }
            }
        }
        if out.is_empty() {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no parts found for {}", first.display()),
            )));
        }
        Ok(out)
    }

    pub fn part_paths(&self) -> impl Iterator<Item = &Path> {
        self.parts.iter().map(|p| p.path.as_path())
    }

    /// Index of the part containing `offset` (or the last part when `offset == len`).
    fn locate(&self, offset: u64) -> usize {
        self.parts
            .partition_point(|p| p.start <= offset)
            .saturating_sub(1)
    }
}

impl Source for MultiPartSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn slice(&self, offset: u64, len: usize) -> Result<Cow<'_, [u8]>> {
        check_range(offset, len as u64, self.len)?;
        if len == 0 {
            return Ok(Cow::Borrowed(&[]));
        }
        let mut idx = self.locate(offset);
        let part = &self.parts[idx];
        let mut local = (offset - part.start) as usize;
        if local + len <= part.len {
            return Ok(Cow::Borrowed(&part.bytes()[local..local + len]));
        }
        let mut out = Vec::with_capacity(len);
        let mut remaining = len;
        while remaining > 0 {
            let part = &self.parts[idx];
            let take = remaining.min(part.len - local);
            out.extend_from_slice(&part.bytes()[local..local + take]);
            remaining -= take;
            idx += 1;
            local = 0;
        }
        Ok(Cow::Owned(out))
    }

    fn contiguous_tail(&self, offset: u64) -> Option<&[u8]> {
        if offset > self.len {
            return None;
        }
        let idx = self.locate(offset);
        if idx + 1 != self.parts.len() {
            return None;
        }
        let part = &self.parts[idx];
        Some(&part.bytes()[(offset - part.start) as usize..])
    }

    fn describe(&self) -> String {
        let first = self
            .parts
            .first()
            .map(|p| p.path.display().to_string())
            .unwrap_or_default();
        format!("{first} (+{} parts)", self.parts.len().saturating_sub(1))
    }
}

/// Sequential `Read` adapter starting at an offset of a [`Source`]; used to feed
/// streaming decompressors when the bytes are not contiguous in memory.
pub struct SourceReader<'a> {
    source: &'a dyn Source,
    pos: u64,
    end: u64,
}

impl fmt::Debug for SourceReader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceReader")
            .field("source", &self.source.describe())
            .field("pos", &self.pos)
            .field("end", &self.end)
            .finish()
    }
}

impl<'a> SourceReader<'a> {
    pub fn new(source: &'a dyn Source, start: u64) -> Self {
        Self {
            source,
            pos: start,
            end: source.len(),
        }
    }
}

impl Read for SourceReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let avail = self.end.saturating_sub(self.pos);
        if avail == 0 || buf.is_empty() {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(avail) as usize;
        self.source
            .read_at(self.pos, &mut buf[..n])
            .map_err(io::Error::other)?;
        self.pos += n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zimz-src-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn memory_source_bounds() {
        let s = MemorySource::new(b"0123456789".to_vec());
        assert_eq!(s.len(), 10);
        assert!(!s.is_empty());
        assert_eq!(&*s.slice(2, 3).unwrap(), b"234");
        assert_eq!(&*s.slice(10, 0).unwrap(), b"");
        assert!(matches!(
            s.slice(8, 3),
            Err(Error::OutOfBounds {
                offset: 8,
                len: 3,
                size: 10
            })
        ));
        assert!(
            matches!(s.slice(u64::MAX, 1), Err(Error::OutOfBounds { .. })),
            "no overflow"
        );
        assert_eq!(s.contiguous_tail(7), Some(&b"789"[..]));
        assert_eq!(s.contiguous_tail(10), Some(&b""[..]));
        assert_eq!(s.contiguous_tail(11), None);
        let mut buf = [0u8; 4];
        s.read_at(3, &mut buf).unwrap();
        assert_eq!(&buf, b"3456");
        assert!(s.describe().contains("10 bytes"));
    }

    #[test]
    fn sub_source_maps_offsets() {
        let inner = MemorySource::new(b"xxABCDEFyy".to_vec());
        let s = SubSource::new(inner, 2, 6).unwrap();
        assert_eq!(s.len(), 6);
        assert_eq!(&*s.slice(0, 3).unwrap(), b"ABC");
        assert_eq!(&*s.slice(3, 3).unwrap(), b"DEF");
        assert!(s.slice(4, 3).is_err(), "cannot see past the sub range");
        assert_eq!(
            s.contiguous_tail(4),
            Some(&b"EF"[..]),
            "tail is clipped to the sub range"
        );
        assert_eq!(s.contiguous_tail(7), None);
        assert!(s.describe().contains("[2..+6]"));
        assert!(SubSource::new(MemorySource::new(vec![0; 4]), 3, 2).is_err());
    }

    #[test]
    fn multipart_slices_within_and_across_parts() {
        let dir = temp_dir("multi");
        let parts = [b"AAAA".to_vec(), b"BBB".to_vec(), b"CC".to_vec()];
        let paths: Vec<PathBuf> = parts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let path = dir.join(format!("part{i}"));
                std::fs::write(&path, p).unwrap();
                path
            })
            .collect();
        let s = MultiPartSource::open(&paths).unwrap();
        assert_eq!(s.len(), 9);
        assert!(matches!(s.slice(1, 2).unwrap(), Cow::Borrowed(b"AA")));
        assert!(matches!(s.slice(3, 3).unwrap(), Cow::Owned(ref v) if v == b"ABB"));
        assert_eq!(&*s.slice(0, 9).unwrap(), b"AAAABBBCC");
        assert_eq!(&*s.slice(9, 0).unwrap(), b"");
        assert!(s.slice(8, 2).is_err());
        assert_eq!(s.contiguous_tail(7), Some(&b"CC"[..]));
        assert_eq!(s.contiguous_tail(9), Some(&b""[..]));
        assert_eq!(s.contiguous_tail(2), None, "spans parts");
        assert_eq!(s.contiguous_tail(10), None);
        assert_eq!(s.part_paths().count(), 3);
        assert!(s.describe().contains("+2 parts"));
        let mut reader = SourceReader::new(&s, 2);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"AABBBCC");
        assert!(format!("{reader:?}").contains("SourceReader"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn multipart_ranges_share_mappings_and_skip_empty_ranges() {
        let dir = temp_dir("ranges");
        let file = dir.join("container");
        std::fs::write(&file, b"__HELLO__WORLD__").unwrap();
        let s = MultiPartSource::open_ranges([
            FileRange {
                path: file.clone(),
                offset: 2,
                len: 5,
            },
            FileRange {
                path: file.clone(),
                offset: 0,
                len: 0,
            },
            FileRange {
                path: file.clone(),
                offset: 9,
                len: 5,
            },
        ])
        .unwrap();
        assert_eq!(s.len(), 10);
        assert_eq!(&*s.slice(0, 10).unwrap(), b"HELLOWORLD");
        assert_eq!(s.part_paths().count(), 2);
        assert!(
            MultiPartSource::open_ranges([FileRange {
                path: file.clone(),
                offset: 12,
                len: 10
            }])
            .is_err(),
            "range past the file"
        );
        assert!(matches!(
            MultiPartSource::open_ranges(Vec::new()),
            Err(Error::Corrupt(_))
        ));
        assert!(MultiPartSource::open::<&PathBuf>([]).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn discover_split_parts() {
        let dir = temp_dir("discover");
        for suffix in ["aa", "ab", "ac", "ba"] {
            std::fs::write(dir.join(format!("x.zim{suffix}")), b"1").unwrap();
        }
        let parts = MultiPartSource::discover(dir.join("x.zimaa")).unwrap();
        assert_eq!(parts.len(), 3, "stops at the first gap (ad)");
        assert!(parts[2].ends_with("x.zimac"));
        assert_eq!(
            MultiPartSource::discover(dir.join("x.zimab"))
                .unwrap()
                .len(),
            3,
            "any part name works"
        );
        assert!(matches!(
            MultiPartSource::discover(dir.join("y.zimaa")),
            Err(Error::Io(_))
        ));
        assert!(matches!(
            MultiPartSource::discover(dir.join("x.zim01")),
            Err(Error::Corrupt(_))
        ));
        assert!(matches!(
            MultiPartSource::discover(dir.join("z")),
            Err(Error::Corrupt(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mmap_source_rejects_empty_and_missing_files() {
        let dir = temp_dir("mmap");
        let empty = dir.join("empty.zim");
        std::fs::write(&empty, b"").unwrap();
        assert!(matches!(MmapSource::open(&empty), Err(Error::Corrupt(_))));
        assert!(matches!(
            MmapSource::open(dir.join("missing")),
            Err(Error::Io(_))
        ));
        let file = dir.join("f");
        std::fs::write(&file, b"abc").unwrap();
        let m = MmapSource::open(&file).unwrap();
        assert_eq!(m.as_bytes(), b"abc");
        assert_eq!(m.path(), file);
        assert_eq!(&*m.slice(1, 2).unwrap(), b"bc");
        assert_eq!(m.contiguous_tail(1), Some(&b"bc"[..]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn check_range_arithmetic() {
        check_range(0, 0, 0).unwrap();
        check_range(5, 5, 10).unwrap();
        assert!(check_range(5, 6, 10).is_err());
        assert!(check_range(u64::MAX, 1, u64::MAX).is_err());
        assert!(check_range(1, u64::MAX, u64::MAX).is_err());
    }
}
