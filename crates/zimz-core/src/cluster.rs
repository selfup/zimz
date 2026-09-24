// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Clusters: one info byte, then a (possibly compressed) payload holding an offset
//! table followed by the blobs.

use crate::codec::decode_to_vec;
use crate::source::{Source, SourceReader};
use crate::{Error, Result};

/// Bit 4 of the info byte: offsets are 8 bytes instead of 4.
pub const EXTENDED_FLAG: u8 = 0x10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compression {
    None,
    /// Obsolete; libzim rejects it too.
    Zlib,
    /// Obsolete; libzim rejects it too.
    Bzip2,
    /// LZMA2 in an XZ container (ZIMs from before ~2021).
    Xz,
    Zstd,
}

impl Compression {
    pub fn from_info_byte(info: u8) -> Result<Self> {
        Ok(match info & 0x0f {
            0 | 1 => Self::None,
            2 => Self::Zlib,
            3 => Self::Bzip2,
            4 => Self::Xz,
            5 => Self::Zstd,
            other => return Err(Error::UnsupportedCompression(other)),
        })
    }

    pub fn code(self) -> u8 {
        match self {
            Self::None => 1,
            Self::Zlib => 2,
            Self::Bzip2 => 3,
            Self::Xz => 4,
            Self::Zstd => 5,
        }
    }

    pub fn is_compressed(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Debug)]
enum Payload {
    /// Decompressed bytes (offset table + blobs).
    Owned(Vec<u8>),
    /// Uncompressed cluster: blobs are read straight from the source at `base + offset`.
    Direct { base: u64 },
}

/// A decoded cluster: its offset table and, for compressed clusters, the payload.
#[derive(Debug)]
pub struct ClusterData {
    index: u32,
    compression: Compression,
    extended: bool,
    /// `blob_count + 1` offsets relative to the payload start.
    offsets: Vec<u64>,
    payload: Payload,
}

impl ClusterData {
    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn compression(&self) -> Compression {
        self.compression
    }

    pub fn is_extended(&self) -> bool {
        self.extended
    }

    /// Uncompressed clusters are served directly from the source, not from memory.
    pub fn is_direct(&self) -> bool {
        matches!(self.payload, Payload::Direct { .. })
    }

    pub fn blob_count(&self) -> u32 {
        (self.offsets.len() - 1) as u32
    }

    /// `(start, end)` of a blob relative to the payload start.
    pub fn blob_range(&self, blob: u32) -> Option<(u64, u64)> {
        let i = blob as usize;
        (i + 1 < self.offsets.len()).then(|| (self.offsets[i], self.offsets[i + 1]))
    }

    pub fn blob_size(&self, blob: u32) -> Option<u64> {
        self.blob_range(blob).map(|(s, e)| e - s)
    }

    /// Total payload size in bytes (decompressed).
    pub fn payload_len(&self) -> u64 {
        self.offsets.last().copied().unwrap_or(0)
    }

    pub(crate) fn owned_bytes(&self) -> Option<&[u8]> {
        match &self.payload {
            Payload::Owned(v) => Some(v),
            Payload::Direct { .. } => None,
        }
    }

    pub(crate) fn direct_base(&self) -> Option<u64> {
        match &self.payload {
            Payload::Direct { base } => Some(*base),
            Payload::Owned(_) => None,
        }
    }

    /// Approximate heap usage, for cache accounting.
    pub fn memory_footprint(&self) -> usize {
        self.offsets.len() * 8 + self.owned_bytes().map_or(0, <[u8]>::len)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ClusterReadConfig {
    /// Refuse to decompress beyond this many bytes.
    pub max_decompressed: u64,
    /// Upper bound on blobs per cluster (libzim uses the entry count).
    pub max_blobs: u64,
}

fn read_le(bytes: &[u8], at: usize, width: usize) -> u64 {
    let mut v = 0u64;
    for (i, b) in bytes[at..at + width].iter().enumerate() {
        v |= u64::from(*b) << (8 * i);
    }
    v
}

/// Parse and validate the offset table at the start of a payload. `payload_len`, when
/// known, bounds every offset.
fn parse_offsets(
    bytes: &[u8],
    width: usize,
    max_blobs: u64,
    payload_len: Option<u64>,
) -> Result<Vec<u64>> {
    if bytes.len() < width {
        return Err(Error::corrupt("cluster is too short for an offset table"));
    }
    let first = read_le(bytes, 0, width);
    let w = width as u64;
    if first != w && first < 2 * w {
        return Err(Error::corrupt(
            "offset of first blob in cluster is too small",
        ));
    }
    if !first.is_multiple_of(w) {
        return Err(Error::corrupt(
            "offset of first blob in cluster is misaligned",
        ));
    }
    let count = first / w;
    if count > max_blobs.saturating_add(1) {
        return Err(Error::corrupt(
            "cluster claims more blobs than the archive has entries",
        ));
    }
    if count.saturating_mul(w) > bytes.len() as u64 {
        return Err(Error::corrupt(
            "offset of first blob in cluster is beyond the cluster",
        ));
    }
    let mut offsets = Vec::with_capacity(count as usize);
    let mut prev = 0u64;
    for i in 0..count as usize {
        let v = read_le(bytes, i * width, width);
        if v < prev {
            return Err(Error::corrupt("blob offsets in cluster are not ordered"));
        }
        if let Some(len) = payload_len
            && v > len
        {
            return Err(Error::corrupt("blob offset points beyond the cluster"));
        }
        offsets.push(v);
        prev = v;
    }
    Ok(offsets)
}

pub(crate) fn read_cluster(
    source: &dyn Source,
    index: u32,
    offset: u64,
    cfg: &ClusterReadConfig,
) -> Result<ClusterData> {
    let info = source.slice(offset, 1)?[0];
    let compression = Compression::from_info_byte(info)?;
    let extended = info & EXTENDED_FLAG != 0;
    let width: usize = if extended { 8 } else { 4 };
    let payload_start = offset + 1;
    if compression.is_compressed() {
        let data = match source.contiguous_tail(payload_start) {
            Some(tail) => decode_to_vec(compression, tail, cfg.max_decompressed)?,
            None => decode_to_vec(
                compression,
                SourceReader::new(source, payload_start),
                cfg.max_decompressed,
            )?,
        };
        let offsets = parse_offsets(&data, width, cfg.max_blobs, Some(data.len() as u64))?;
        Ok(ClusterData {
            index,
            compression,
            extended,
            offsets,
            payload: Payload::Owned(data),
        })
    } else {
        let remaining = source.len().saturating_sub(payload_start);
        let head = source.slice(payload_start, width.min(remaining as usize))?;
        if head.len() < width {
            return Err(Error::corrupt("cluster is too short for an offset table"));
        }
        let first = read_le(&head, 0, width);
        let w = width as u64;
        if !first.is_multiple_of(w) || first == 0 || first / w * w > remaining {
            return Err(Error::corrupt("offset of first blob in cluster is invalid"));
        }
        let table = source.slice(payload_start, (first / w * w) as usize)?;
        let offsets = parse_offsets(&table, width, cfg.max_blobs, Some(remaining))?;
        Ok(ClusterData {
            index,
            compression,
            extended,
            offsets,
            payload: Payload::Direct {
                base: payload_start,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;

    fn cfg() -> ClusterReadConfig {
        ClusterReadConfig {
            max_decompressed: 1 << 20,
            max_blobs: 100,
        }
    }

    #[test]
    fn uncompressed_cluster() {
        // info byte 1, offsets [12, 25, 29], then "<h1>Auto</h1>" and "Auto".
        let mut c = vec![1u8];
        for o in [12u32, 25, 29] {
            c.extend_from_slice(&o.to_le_bytes());
        }
        c.extend_from_slice(b"<h1>Auto</h1>Auto");
        let src = MemorySource::new(c);
        let d = read_cluster(&src, 0, 0, &cfg()).unwrap();
        assert!(d.is_direct());
        assert_eq!(d.blob_count(), 2);
        assert_eq!(d.blob_range(0), Some((12, 25)));
        assert_eq!(d.blob_range(1), Some((25, 29)));
        assert_eq!(d.blob_range(2), None);
        assert_eq!(d.compression(), Compression::None);
    }

    #[test]
    fn rejects_bad_offsets() {
        let mk = |offs: &[u32]| {
            let mut c = vec![1u8];
            for o in offs {
                c.extend_from_slice(&o.to_le_bytes());
            }
            c.extend_from_slice(&[0u8; 64]);
            MemorySource::new(c)
        };
        assert!(
            read_cluster(&mk(&[0]), 0, 0, &cfg()).is_err(),
            "zero first offset"
        );
        assert!(
            read_cluster(&mk(&[6, 10]), 0, 0, &cfg()).is_err(),
            "misaligned"
        );
        assert!(
            read_cluster(&mk(&[12, 8, 20]), 0, 0, &cfg()).is_err(),
            "unordered"
        );
        assert!(
            read_cluster(&mk(&[4]), 0, 0, &cfg()).is_ok(),
            "zero blobs is legal"
        );
        assert!(
            read_cluster(&mk(&[4000]), 0, 0, &cfg()).is_err(),
            "table beyond cluster"
        );
        let mut c = vec![2u8];
        c.extend_from_slice(&[0u8; 16]);
        assert!(matches!(
            read_cluster(&MemorySource::new(c), 0, 0, &cfg()),
            Err(Error::UnsupportedCompression(2))
        ));
    }

    #[cfg(feature = "zstd-c")]
    #[test]
    fn zstd_cluster_roundtrip() {
        let mut payload = Vec::new();
        for o in [12u32, 17, 20] {
            payload.extend_from_slice(&o.to_le_bytes());
        }
        payload.extend_from_slice(b"helloabc");
        let compressed = zstd::bulk::compress(&payload, 3).unwrap();
        let mut c = vec![5u8];
        c.extend_from_slice(&compressed);
        c.extend_from_slice(b"trailing bytes of the next cluster");
        let src = MemorySource::new(c);
        let d = read_cluster(&src, 3, 0, &cfg()).unwrap();
        assert!(!d.is_direct());
        assert_eq!(d.blob_count(), 2);
        assert_eq!(&d.owned_bytes().unwrap()[12..17], b"hello");
        assert_eq!(d.payload_len(), 20);
        let small = ClusterReadConfig {
            max_decompressed: 10,
            max_blobs: 100,
        };
        assert!(matches!(
            read_cluster(&src, 3, 0, &small),
            Err(Error::ClusterTooLarge { .. })
        ));
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;
    use crate::source::MemorySource;

    fn cfg() -> ClusterReadConfig {
        ClusterReadConfig {
            max_decompressed: 1 << 20,
            max_blobs: 100,
        }
    }

    #[test]
    fn info_byte_codes() {
        assert_eq!(Compression::from_info_byte(0).unwrap(), Compression::None);
        assert_eq!(Compression::from_info_byte(1).unwrap(), Compression::None);
        assert_eq!(Compression::from_info_byte(2).unwrap(), Compression::Zlib);
        assert_eq!(Compression::from_info_byte(3).unwrap(), Compression::Bzip2);
        assert_eq!(Compression::from_info_byte(4).unwrap(), Compression::Xz);
        assert_eq!(Compression::from_info_byte(5).unwrap(), Compression::Zstd);
        assert_eq!(
            Compression::from_info_byte(0x15).unwrap(),
            Compression::Zstd,
            "extended flag is separate"
        );
        for code in 6..=15u8 {
            assert!(
                matches!(Compression::from_info_byte(code), Err(Error::UnsupportedCompression(c)) if c == code)
            );
        }
        for c in [
            Compression::None,
            Compression::Zlib,
            Compression::Bzip2,
            Compression::Xz,
            Compression::Zstd,
        ] {
            assert_eq!(Compression::from_info_byte(c.code()).unwrap(), c);
            assert_eq!(c.is_compressed(), c != Compression::None);
        }
    }

    #[test]
    fn extended_uncompressed_cluster() {
        let mut c = vec![1u8 | EXTENDED_FLAG];
        for o in [24u64, 27, 30] {
            c.extend_from_slice(&o.to_le_bytes());
        }
        c.extend_from_slice(b"abcdef");
        let src = MemorySource::new(c);
        let d = read_cluster(&src, 4, 0, &cfg()).unwrap();
        assert!(d.is_extended() && d.is_direct());
        assert_eq!(d.index(), 4);
        assert_eq!(d.blob_count(), 2);
        assert_eq!(d.blob_range(1), Some((27, 30)));
        assert_eq!(d.blob_size(0), Some(3));
        assert_eq!(d.blob_size(2), None);
        assert_eq!(d.payload_len(), 30);
        assert_eq!(d.memory_footprint(), 3 * 8);
        assert!(d.owned_bytes().is_none());
        assert_eq!(d.direct_base(), Some(1));
    }

    #[test]
    fn blob_count_is_bounded_by_the_archive() {
        let mut c = vec![1u8];
        for o in [16u32, 16, 16, 16] {
            c.extend_from_slice(&o.to_le_bytes());
        }
        let src = MemorySource::new(c);
        assert!(read_cluster(&src, 0, 0, &cfg()).is_ok());
        let tight = ClusterReadConfig {
            max_decompressed: 1 << 20,
            max_blobs: 2,
        };
        assert!(matches!(
            read_cluster(&src, 0, 0, &tight),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn uncompressed_table_beyond_the_file() {
        let mut c = vec![1u8];
        c.extend_from_slice(&40u32.to_le_bytes());
        c.extend_from_slice(&[0; 8]);
        let src = MemorySource::new(c);
        assert!(matches!(
            read_cluster(&src, 0, 0, &cfg()),
            Err(Error::Corrupt(_))
        ));
        let src = MemorySource::new(vec![1u8, 4]);
        assert!(
            matches!(read_cluster(&src, 0, 0, &cfg()), Err(Error::Corrupt(_))),
            "short table"
        );
        let src = MemorySource::new(vec![1u8]);
        assert!(read_cluster(&src, 0, 0, &cfg()).is_err(), "no table at all");
        assert!(
            matches!(
                read_cluster(&src, 0, 5, &cfg()),
                Err(Error::OutOfBounds { .. })
            ),
            "info byte beyond file"
        );
    }

    #[test]
    fn parse_offsets_rules() {
        let tbl = |offs: &[u32]| {
            offs.iter()
                .flat_map(|o| o.to_le_bytes())
                .collect::<Vec<u8>>()
        };
        assert_eq!(parse_offsets(&tbl(&[4]), 4, 10, Some(4)).unwrap(), vec![4]);
        assert_eq!(
            parse_offsets(&tbl(&[8, 8]), 4, 10, Some(8)).unwrap(),
            vec![8, 8],
            "zero-length blob"
        );
        assert!(
            parse_offsets(&tbl(&[8, 9]), 4, 10, Some(8)).is_err(),
            "beyond payload"
        );
        assert!(
            parse_offsets(&tbl(&[8, 9]), 4, 10, None).is_ok(),
            "unbounded when payload length unknown"
        );
        assert!(
            parse_offsets(&tbl(&[12, 8, 20]), 4, 10, None).is_err(),
            "unordered"
        );
        assert!(
            parse_offsets(&tbl(&[5, 8]), 4, 10, None).is_err(),
            "misaligned"
        );
        assert!(parse_offsets(&tbl(&[0]), 4, 10, None).is_err(), "zero");
        assert!(parse_offsets(&[1, 2], 4, 10, None).is_err(), "too short");
    }

    #[cfg(feature = "zstd-c")]
    #[test]
    fn compressed_cluster_with_bad_table_is_rejected() {
        let mut payload = Vec::new();
        for o in [12u32, 100, 20] {
            payload.extend_from_slice(&o.to_le_bytes());
        }
        payload.extend_from_slice(b"12345678");
        let mut c = vec![5u8];
        c.extend_from_slice(&zstd::bulk::compress(&payload, 1).unwrap());
        assert!(matches!(
            read_cluster(&MemorySource::new(c), 0, 0, &cfg()),
            Err(Error::Corrupt(_))
        ));
        let mut garbage = vec![5u8];
        garbage.extend_from_slice(b"this is not zstd");
        assert!(matches!(
            read_cluster(&MemorySource::new(garbage), 0, 0, &cfg()),
            Err(Error::Decode(_))
        ));
    }
}
