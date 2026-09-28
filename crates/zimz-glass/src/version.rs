// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! The version block at the start of a single-file glass database.

use crate::pack::Reader;
use crate::{Error, Result};

pub const MAGIC: &[u8; 14] = b"\x0f\x0dXapian Glass";
/// `DATE_TO_VERSION(2016, 3, 14)`: the only glass format version ever released.
pub const FORMAT_VERSION: u16 = 0x046e;

/// Table order in the version block and in `Glass::table_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    PostList = 0,
    DocData = 1,
    TermList = 2,
    Position = 3,
    Spelling = 4,
    Synonym = 5,
}

/// Root block and shape of one B-tree table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootInfo {
    pub root: u32,
    pub level: u8,
    pub sequential: bool,
    pub root_is_fake: bool,
    pub num_entries: u64,
    pub block_size: u32,
    pub compress_min: u32,
}

impl RootInfo {
    fn parse(r: &mut Reader<'_>) -> Result<Self> {
        let root = r.u32()?;
        let val = r.uint()?;
        let num_entries = r.uint()?;
        let b = r.uint()?;
        let compress_min = r.u32()?;
        let _freelist = r.string()?;
        let level = val >> 2;
        if level >= 10 {
            return Err(Error::corrupt("impossibly deep B-tree"));
        }
        let root_is_fake = val & 1 != 0;
        if root_is_fake && level > 0 {
            return Err(Error::corrupt("fake root but level > 0"));
        }
        let block_size = b << 11;
        if !(2048..=65536).contains(&block_size) || !block_size.is_power_of_two() {
            return Err(Error::corrupt("invalid block size"));
        }
        Ok(Self {
            root,
            level: level as u8,
            sequential: val & 2 != 0,
            root_is_fake,
            num_entries,
            block_size: block_size as u32,
            compress_min: if compress_min == 4 { 18 } else { compress_min },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub doc_count: u32,
    pub last_docid: u32,
    pub doclen_lower_bound: u32,
    pub doclen_upper_bound: u32,
    pub wdf_upper_bound: u32,
    pub oldest_changeset: u64,
    pub total_doclen: u64,
    pub spelling_wordfreq_upper_bound: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionBlock {
    pub uuid: [u8; 16],
    pub revision: u64,
    pub roots: [RootInfo; 6],
    pub stats: Stats,
}

impl VersionBlock {
    /// Parse from the first bytes of the database (Xapian reads 256).
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 16 + 16 || &data[..14] != MAGIC {
            return Err(Error::BadMagic);
        }
        let version = u16::from_be_bytes([data[14], data[15]]);
        if version != FORMAT_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&data[16..32]);
        let mut r = Reader::new(&data[32..data.len().min(4096)]);
        let revision = r.uint()?;
        let mut roots = [RootInfo {
            root: 0,
            level: 0,
            sequential: false,
            root_is_fake: true,
            num_entries: 0,
            block_size: 8192,
            compress_min: 18,
        }; 6];
        for root in &mut roots {
            *root = RootInfo::parse(&mut r)?;
        }
        // Stats run to the end of the version data; in a single-file DB block 1 follows,
        // so only the eight values are read (the rest of the block is padding/tables).
        let stats = if r.at_end() {
            Stats::default()
        } else {
            let doc_count = r.u32()?;
            let last_docid = r
                .u32()?
                .checked_add(doc_count)
                .ok_or_else(|| Error::corrupt("last docid overflow"))?;
            let doclen_lower_bound = r.u32()?;
            let wdf_upper_bound = r.u32()?;
            let doclen_upper_bound = r
                .u32()?
                .checked_add(wdf_upper_bound)
                .ok_or_else(|| Error::corrupt("doclen bound overflow"))?;
            let oldest_changeset = r.uint()?;
            let total_doclen = r.uint()?;
            let spelling_wordfreq_upper_bound = r.u32()?;
            Stats {
                doc_count,
                last_docid,
                doclen_lower_bound,
                doclen_upper_bound,
                wdf_upper_bound,
                oldest_changeset,
                total_doclen,
                spelling_wordfreq_upper_bound,
            }
        };
        Ok(Self {
            uuid,
            revision,
            roots,
            stats,
        })
    }

    pub fn root(&self, kind: TableKind) -> &RootInfo {
        &self.roots[kind as usize]
    }

    /// The block size shared by every table (libzim never overrides the 8 KiB default).
    pub fn block_size(&self) -> u32 {
        self.roots[TableKind::PostList as usize].block_size
    }

    pub fn uuid_string(&self) -> String {
        use std::fmt::Write as _;
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

    #[test]
    fn rejects_bad_headers() {
        assert!(matches!(
            VersionBlock::parse(b"not a database at all, definitely not"),
            Err(Error::BadMagic)
        ));
        let mut b = MAGIC.to_vec();
        b.extend_from_slice(&[0x04, 0x70]);
        b.extend_from_slice(&[0; 32]);
        assert!(matches!(
            VersionBlock::parse(&b),
            Err(Error::UnsupportedVersion(0x0470))
        ));
        let mut b = MAGIC.to_vec();
        b.extend_from_slice(&[0x04, 0x6e]);
        b.extend_from_slice(&[7; 16]);
        b.push(1); // revision
        assert!(VersionBlock::parse(&b).is_err(), "truncated root infos");
    }
}
