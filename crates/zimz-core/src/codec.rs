// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Streaming cluster decompression. The ZIM format stores no compressed length, so a
//! decoder is handed "everything from the payload start to the end of the archive" and
//! stops at the end of the first frame/stream.

use std::io::Read;

use crate::cluster::Compression;
use crate::{Error, Result};

/// Largest zstd window accepted (matches libzstd's default `ZSTD_WINDOWLOG_LIMIT_DEFAULT`
/// of 2^27; some 5.0-era archives were written at level 22 and need exactly this).
#[cfg(all(feature = "zstd-pure", not(feature = "zstd-c")))]
const ZSTD_MAX_WINDOW: u64 = 1 << 27;
/// Decoder memory allowed for one xz cluster. libzim wrote xz clusters with preset 9
/// (64 MiB dictionary); the limit stops a corrupt header from forcing a huge allocation.
#[cfg(any(feature = "xz-c", feature = "xz-pure"))]
const XZ_MEM_LIMIT: u64 = 128 << 20;

pub(crate) fn decode_to_vec(
    compression: Compression,
    reader: impl Read,
    limit: u64,
) -> Result<Vec<u8>> {
    match compression {
        Compression::None => read_limited(reader, limit),
        Compression::Zstd => read_limited(zstd_decoder(reader)?, limit),
        Compression::Xz => read_limited(xz_decoder(reader)?, limit),
        Compression::Zlib | Compression::Bzip2 => {
            Err(Error::UnsupportedCompression(compression.code()))
        }
    }
}

fn read_limited(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| Error::Decode(e.to_string()))?;
    if out.len() as u64 > limit {
        return Err(Error::ClusterTooLarge {
            size: out.len() as u64,
            limit,
        });
    }
    Ok(out)
}

#[cfg(feature = "zstd-c")]
fn zstd_decoder<'a>(reader: impl Read + 'a) -> Result<Box<dyn Read + 'a>> {
    let decoder =
        zstd::stream::read::Decoder::new(reader).map_err(|e| Error::Decode(e.to_string()))?;
    Ok(Box::new(decoder.single_frame()))
}

// ruzstd caps the window at 100 MB by default, below what level-22 archives need.
#[cfg(all(feature = "zstd-pure", not(feature = "zstd-c")))]
#[allow(clippy::unnecessary_wraps)]
fn zstd_decoder<'a>(reader: impl Read + 'a) -> Result<Box<dyn Read + 'a>> {
    let decoder =
        ruzstd::decoding::StreamingDecoder::new_with_max_window_size(reader, ZSTD_MAX_WINDOW)
            .map_err(|e| Error::Decode(e.to_string()))?;
    Ok(Box::new(decoder))
}

#[cfg(not(any(feature = "zstd-c", feature = "zstd-pure")))]
fn zstd_decoder<'a>(_reader: impl Read + 'a) -> Result<Box<dyn Read + 'a>> {
    Err(Error::UnsupportedCompression(Compression::Zstd.code()))
}

#[cfg(feature = "xz-c")]
fn xz_decoder<'a>(reader: impl Read + 'a) -> Result<Box<dyn Read + 'a>> {
    let stream = liblzma::stream::Stream::new_stream_decoder(XZ_MEM_LIMIT, 0)
        .map_err(|e| Error::Decode(e.to_string()))?;
    Ok(Box::new(liblzma::read::XzDecoder::new_stream(
        reader, stream,
    )))
}

#[cfg(all(feature = "xz-pure", not(feature = "xz-c")))]
#[allow(clippy::unnecessary_wraps)]
fn xz_decoder<'a>(reader: impl Read + 'a) -> Result<Box<dyn Read + 'a>> {
    let limit_kb = (XZ_MEM_LIMIT >> 10) as u32;
    Ok(Box::new(lzma_rust2::XzReader::new_mem_limit(
        reader, false, limit_kb,
    )))
}

#[cfg(not(any(feature = "xz-c", feature = "xz-pure")))]
fn xz_decoder<'a>(_reader: impl Read + 'a) -> Result<Box<dyn Read + 'a>> {
    Err(Error::UnsupportedCompression(Compression::Xz.code()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_and_limits() {
        assert_eq!(
            decode_to_vec(Compression::None, &b"abc"[..], 3).unwrap(),
            b"abc"
        );
        assert!(matches!(
            decode_to_vec(Compression::None, &b"abcd"[..], 3),
            Err(Error::ClusterTooLarge { size: 4, limit: 3 })
        ));
        assert_eq!(
            decode_to_vec(Compression::None, &b""[..], 0).unwrap(),
            [] as [u8; 0]
        );
        assert!(matches!(
            decode_to_vec(Compression::Zlib, &b"x"[..], 10),
            Err(Error::UnsupportedCompression(2))
        ));
        assert!(matches!(
            decode_to_vec(Compression::Bzip2, &b"x"[..], 10),
            Err(Error::UnsupportedCompression(3))
        ));
    }

    #[cfg(any(feature = "zstd-c", feature = "zstd-pure"))]
    #[test]
    fn zstd_garbage_is_a_decode_error() {
        assert!(matches!(
            decode_to_vec(Compression::Zstd, &b"definitely not zstd"[..], 1000),
            Err(Error::Decode(_))
        ));
    }

    #[cfg(any(feature = "xz-c", feature = "xz-pure"))]
    #[test]
    fn xz_garbage_is_a_decode_error() {
        assert!(matches!(
            decode_to_vec(Compression::Xz, &b"definitely not xz"[..], 1000),
            Err(Error::Decode(_))
        ));
    }

    #[cfg(feature = "zstd-c")]
    #[test]
    fn zstd_stops_at_the_frame_end_and_honours_the_limit() {
        let data = b"hello hello hello".repeat(20);
        let mut input = zstd::bulk::compress(&data, 1).unwrap();
        input.extend_from_slice(b"garbage after the frame");
        assert_eq!(
            decode_to_vec(Compression::Zstd, &input[..], data.len() as u64).unwrap(),
            data
        );
        assert!(matches!(
            decode_to_vec(Compression::Zstd, &input[..], data.len() as u64 - 1),
            Err(Error::ClusterTooLarge { .. })
        ));
    }

    #[cfg(feature = "xz-c")]
    #[test]
    fn xz_roundtrip_with_trailing_bytes() {
        use std::io::Write;
        let data = b"xz xz xz".repeat(50);
        let mut enc = liblzma::write::XzEncoder::new(Vec::new(), 3);
        enc.write_all(&data).unwrap();
        let mut input = enc.finish().unwrap();
        input.extend_from_slice(b"next cluster");
        assert_eq!(
            decode_to_vec(Compression::Xz, &input[..], 1 << 20).unwrap(),
            data
        );
    }
}
