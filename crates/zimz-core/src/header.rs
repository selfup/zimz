// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! The 80-byte ZIM header.

use std::fmt::Write as _;

use crate::{Error, Result};

pub const HEADER_SIZE: usize = 80;
pub const MAGIC: u32 = 0x044D_495A;
/// `titlePtrPos` value meaning "no title pointer list" (ZIM 6.3+).
pub const NO_TITLE_LIST: u64 = u64::MAX;
/// `mainPage` / `layoutPage` value meaning "none".
pub const NO_PAGE: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub major: u16,
    pub minor: u16,
    pub uuid: [u8; 16],
    pub entry_count: u32,
    pub cluster_count: u32,
    pub path_ptr_pos: u64,
    pub title_ptr_pos: u64,
    pub cluster_ptr_pos: u64,
    pub mime_list_pos: u64,
    pub main_page: u32,
    pub layout_page: u32,
    pub checksum_pos: u64,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

impl Header {
    /// Parse the header from the first bytes of an archive.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_SIZE {
            return Err(Error::corrupt("file is smaller than the ZIM header"));
        }
        if u32_at(bytes, 0) != MAGIC {
            return Err(Error::BadMagic);
        }
        let major = u16_at(bytes, 4);
        if major != 5 && major != 6 {
            return Err(Error::UnsupportedVersion(major));
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&bytes[8..24]);
        Ok(Self {
            major,
            minor: u16_at(bytes, 6),
            uuid,
            entry_count: u32_at(bytes, 24),
            cluster_count: u32_at(bytes, 28),
            path_ptr_pos: u64_at(bytes, 32),
            title_ptr_pos: u64_at(bytes, 40),
            cluster_ptr_pos: u64_at(bytes, 48),
            mime_list_pos: u64_at(bytes, 56),
            main_page: u32_at(bytes, 64),
            layout_page: u32_at(bytes, 68),
            checksum_pos: u64_at(bytes, 72),
        })
    }

    /// Sanity checks mirroring libzim's, plus bounds checks on the pointer lists.
    pub fn validate(&self, file_len: u64) -> Result<()> {
        if self.mime_list_pos != 80 && self.mime_list_pos != 72 {
            return Err(Error::corrupt("mimeListPos must be 80"));
        }
        if (self.entry_count == 0) != (self.cluster_count == 0) {
            return Err(Error::corrupt(
                "entry count and cluster count must both be zero or both non-zero",
            ));
        }
        if self.cluster_count > self.entry_count {
            return Err(Error::corrupt("more clusters than entries"));
        }
        if self.path_ptr_pos < self.mime_list_pos {
            return Err(Error::corrupt("pathPtrPos must be after the MIME list"));
        }
        if self.cluster_ptr_pos < self.mime_list_pos {
            return Err(Error::corrupt("clusterPtrPos must be after the MIME list"));
        }
        if self.has_title_pointer_list() && self.title_ptr_pos < self.mime_list_pos {
            return Err(Error::corrupt("titlePtrPos must be after the MIME list"));
        }
        if self.has_checksum() {
            match self.checksum_pos.checked_add(16) {
                Some(end) if end == file_len => {}
                _ => {
                    return Err(Error::corrupt(
                        "archive size does not match checksumPos (truncated or corrupt)",
                    ));
                }
            }
        }
        let list_end = |pos: u64, width: u64, count: u32| {
            pos.checked_add(width.saturating_mul(u64::from(count)))
        };
        match list_end(self.path_ptr_pos, 8, self.entry_count) {
            Some(end) if end <= file_len => {}
            _ => return Err(Error::corrupt("path pointer list is out of bounds")),
        }
        match list_end(self.cluster_ptr_pos, 8, self.cluster_count) {
            Some(end) if end <= file_len => {}
            _ => return Err(Error::corrupt("cluster pointer list is out of bounds")),
        }
        if self.has_title_pointer_list() {
            match list_end(self.title_ptr_pos, 4, self.entry_count) {
                Some(end) if end <= file_len => {}
                _ => return Err(Error::corrupt("title pointer list is out of bounds")),
            }
        }
        if self.has_main_page() && self.main_page >= self.entry_count {
            return Err(Error::corrupt("main page index is out of bounds"));
        }
        Ok(())
    }

    /// ZIM minor version ≥ 1: user content lives in the `C` namespace.
    pub fn uses_new_namespace_scheme(&self) -> bool {
        self.minor >= 1
    }

    pub fn has_title_pointer_list(&self) -> bool {
        self.title_ptr_pos != NO_TITLE_LIST
    }

    pub fn has_main_page(&self) -> bool {
        self.main_page != NO_PAGE
    }

    pub fn has_checksum(&self) -> bool {
        self.mime_list_pos >= 80
    }

    /// Hyphenated hex UUID (`8-4-4-4-12`).
    pub fn uuid_string(&self) -> String {
        let hex = self
            .uuid
            .iter()
            .fold(String::with_capacity(32), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            });
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut b = vec![0u8; 80];
        b[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        b[4..6].copy_from_slice(&6u16.to_le_bytes());
        b[6..8].copy_from_slice(&3u16.to_le_bytes());
        b[24..28].copy_from_slice(&3u32.to_le_bytes());
        b[28..32].copy_from_slice(&1u32.to_le_bytes());
        b[32..40].copy_from_slice(&200u64.to_le_bytes());
        b[40..48].copy_from_slice(&NO_TITLE_LIST.to_le_bytes());
        b[48..56].copy_from_slice(&224u64.to_le_bytes());
        b[56..64].copy_from_slice(&80u64.to_le_bytes());
        b[64..68].copy_from_slice(&NO_PAGE.to_le_bytes());
        b[68..72].copy_from_slice(&NO_PAGE.to_le_bytes());
        b[72..80].copy_from_slice(&(300u64).to_le_bytes());
        b
    }

    #[test]
    fn parses_and_validates() {
        let h = Header::parse(&sample()).unwrap();
        assert_eq!(h.major, 6);
        assert_eq!(h.minor, 3);
        assert!(h.uses_new_namespace_scheme());
        assert!(!h.has_title_pointer_list());
        assert!(!h.has_main_page());
        h.validate(316).unwrap();
        assert!(h.validate(317).is_err(), "size mismatch must fail");
        assert_eq!(h.uuid_string().len(), 36);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(Header::parse(&[0u8; 40]), Err(Error::Corrupt(_))));
        let mut b = sample();
        b[0] = 0;
        assert!(matches!(Header::parse(&b), Err(Error::BadMagic)));
        let mut b = sample();
        b[4] = 7;
        assert!(matches!(
            Header::parse(&b),
            Err(Error::UnsupportedVersion(7))
        ));
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn valid(file_len: u64) -> Header {
        Header {
            major: 6,
            minor: 1,
            uuid: [1; 16],
            entry_count: 10,
            cluster_count: 2,
            path_ptr_pos: 1000,
            title_ptr_pos: 1080,
            cluster_ptr_pos: 1120,
            mime_list_pos: 80,
            main_page: 3,
            layout_page: NO_PAGE,
            checksum_pos: file_len - 16,
        }
    }

    #[test]
    fn accepts_a_consistent_header() {
        valid(2000).validate(2000).unwrap();
    }

    #[test]
    fn ancient_header_without_checksum() {
        let mut h = valid(2000);
        h.mime_list_pos = 72;
        h.checksum_pos = 0;
        assert!(!h.has_checksum());
        h.validate(2000).unwrap();
        h.validate(5000).unwrap();
        h.mime_list_pos = 79;
        assert!(h.validate(2000).is_err());
    }

    #[test]
    fn rejects_each_inconsistency() {
        type Mutation = Box<dyn Fn(&mut Header)>;
        let cases: Vec<(&str, Mutation)> = vec![
            (
                "entries without clusters",
                Box::new(|h| h.cluster_count = 0),
            ),
            ("clusters without entries", Box::new(|h| h.entry_count = 0)),
            (
                "more clusters than entries",
                Box::new(|h| h.cluster_count = 11),
            ),
            (
                "path list before mime list",
                Box::new(|h| h.path_ptr_pos = 60),
            ),
            (
                "cluster list before mime list",
                Box::new(|h| h.cluster_ptr_pos = 60),
            ),
            (
                "title list before mime list",
                Box::new(|h| h.title_ptr_pos = 60),
            ),
            ("checksum size mismatch", Box::new(|h| h.checksum_pos += 1)),
            (
                "checksum overflow",
                Box::new(|h| h.checksum_pos = u64::MAX - 3),
            ),
            (
                "path list past the end",
                Box::new(|h| h.path_ptr_pos = 1990),
            ),
            (
                "path list overflow",
                Box::new(|h| h.path_ptr_pos = u64::MAX - 10),
            ),
            (
                "cluster list past the end",
                Box::new(|h| h.cluster_ptr_pos = 1990),
            ),
            (
                "title list past the end",
                Box::new(|h| h.title_ptr_pos = 1990),
            ),
            ("main page out of range", Box::new(|h| h.main_page = 10)),
        ];
        for (name, mutate) in cases {
            let mut h = valid(2000);
            mutate(&mut h);
            assert!(
                matches!(h.validate(2000), Err(Error::Corrupt(_))),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn absent_title_list_and_main_page_are_fine() {
        let mut h = valid(2000);
        h.title_ptr_pos = NO_TITLE_LIST;
        h.main_page = NO_PAGE;
        assert!(!h.has_title_pointer_list());
        assert!(!h.has_main_page());
        h.validate(2000).unwrap();
    }

    #[test]
    fn version_semantics() {
        let mut h = valid(2000);
        h.minor = 0;
        assert!(!h.uses_new_namespace_scheme());
        for minor in 1..=3 {
            h.minor = minor;
            assert!(h.uses_new_namespace_scheme());
        }
    }

    #[test]
    fn parse_reads_every_field() {
        let mut b = vec![0u8; 80];
        b[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        b[4..6].copy_from_slice(&5u16.to_le_bytes());
        b[6..8].copy_from_slice(&0u16.to_le_bytes());
        b[8..24].copy_from_slice(&[0xab; 16]);
        b[24..28].copy_from_slice(&123u32.to_le_bytes());
        b[28..32].copy_from_slice(&45u32.to_le_bytes());
        b[32..40].copy_from_slice(&1u64.to_le_bytes());
        b[40..48].copy_from_slice(&2u64.to_le_bytes());
        b[48..56].copy_from_slice(&3u64.to_le_bytes());
        b[56..64].copy_from_slice(&80u64.to_le_bytes());
        b[64..68].copy_from_slice(&6u32.to_le_bytes());
        b[68..72].copy_from_slice(&7u32.to_le_bytes());
        b[72..80].copy_from_slice(&8u64.to_le_bytes());
        b.extend_from_slice(b"tail");
        let h = Header::parse(&b).unwrap();
        assert_eq!(
            (h.major, h.minor, h.entry_count, h.cluster_count),
            (5, 0, 123, 45)
        );
        assert_eq!(
            (
                h.path_ptr_pos,
                h.title_ptr_pos,
                h.cluster_ptr_pos,
                h.mime_list_pos
            ),
            (1, 2, 3, 80)
        );
        assert_eq!((h.main_page, h.layout_page, h.checksum_pos), (6, 7, 8));
        assert_eq!(h.uuid_string(), "abababab-abab-abab-abab-abababababab");
    }
}
