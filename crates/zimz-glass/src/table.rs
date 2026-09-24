// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Read-only glass B-tree tables (`backends/glass/glass_table.{h,cc}`,
//! `glass_cursor.cc`). All tables of a single-file database share one block-number
//! space: block `n` lives at byte `n * block_size` of the database.

use std::io::Read;

use crate::version::RootInfo;
use crate::{Error, Result};

const DIR_START: usize = 11;
const D2: usize = 2;
const I2: usize = 2;
const K1: usize = 1;
const X2: usize = 2;
const BYTES_PER_BLOCK_NUMBER: usize = 4;
const I_COMPRESSED_BIT: u8 = 0x80;
const I_LAST_BIT: u8 = 0x40;
const I_FIRST_BIT: u8 = 0x20;
const ITEM_SIZE_MASK: u16 = 0x1fff;
pub const MAX_KEY_LEN: usize = 255;

fn read2(b: &[u8], at: usize) -> Result<usize> {
    match b.get(at..at + 2) {
        Some(s) => Ok(usize::from(u16::from_be_bytes([s[0], s[1]]))),
        None => Err(Error::corrupt("read past the end of a block")),
    }
}

fn read4(b: &[u8], at: usize) -> Result<u32> {
    match b.get(at..at + 4) {
        Some(s) => Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]])),
        None => Err(Error::corrupt("read past the end of a block")),
    }
}

/// A leaf item: key, component number, flags and one chunk of the tag.
#[derive(Debug, Clone, Copy)]
struct LeafItem<'a> {
    flags: u8,
    key: &'a [u8],
    component: u16,
    chunk: &'a [u8],
}

impl LeafItem<'_> {
    fn first(&self) -> bool {
        self.flags & I_FIRST_BIT != 0
    }
    fn last(&self) -> bool {
        self.flags & I_LAST_BIT != 0
    }
    fn compressed(&self) -> bool {
        self.flags & I_COMPRESSED_BIT != 0
    }
}

fn leaf_item(block: &[u8], c: usize) -> Result<LeafItem<'_>> {
    let off = read2(block, c)?;
    let i = read2(block, off)? as u16;
    let size = usize::from(i & ITEM_SIZE_MASK) + 3;
    let flags = block[off];
    let item = block
        .get(off..off + size)
        .ok_or_else(|| Error::corrupt("leaf item extends outside its block"))?;
    let klen = usize::from(item[I2]);
    let mut cd = I2 + K1 + klen;
    if cd > size {
        return Err(Error::corrupt("leaf item key extends outside the item"));
    }
    let key = &item[I2 + K1..cd];
    let component = if flags & I_FIRST_BIT != 0 {
        1
    } else {
        let x = read2(item, cd)? as u16;
        cd += X2;
        x
    };
    if cd > size {
        return Err(Error::corrupt(
            "leaf item component field extends outside the item",
        ));
    }
    Ok(LeafItem {
        flags,
        key,
        component,
        chunk: &item[cd..],
    })
}

#[derive(Debug, Clone, Copy)]
struct BranchItem<'a> {
    child: u32,
    key: &'a [u8],
    component: u16,
}

fn branch_item(block: &[u8], c: usize) -> Result<BranchItem<'_>> {
    let off = read2(block, c)?;
    let child = read4(block, off)?;
    let klen = usize::from(
        *block
            .get(off + BYTES_PER_BLOCK_NUMBER)
            .ok_or_else(|| Error::corrupt("branch item outside its block"))?,
    );
    let kstart = off + BYTES_PER_BLOCK_NUMBER + K1;
    let key = block
        .get(kstart..kstart + klen)
        .ok_or_else(|| Error::corrupt("branch key outside its block"))?;
    let component = read2(block, kstart + klen)? as u16;
    Ok(BranchItem {
        child,
        key,
        component,
    })
}

/// Xapian's `compare()`: bytes of the common prefix, then length, then component.
fn compare(key_a: &[u8], comp_a: u16, key_b: &[u8], comp_b: u16) -> std::cmp::Ordering {
    let n = key_a.len().min(key_b.len());
    key_a[..n]
        .cmp(&key_b[..n])
        .then(key_a.len().cmp(&key_b.len()))
        .then(comp_a.cmp(&comp_b))
}

#[derive(Debug, Clone, Copy)]
struct Level {
    block: u32,
    c: usize,
}

/// A cursor over one table. Search keys are compared as component 1 (`form_key`).
#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    data: &'a [u8],
    block_size: usize,
    root: u32,
    level: usize,
    empty: bool,
    /// `levels[0]` is the leaf, `levels[level]` the root.
    levels: Vec<Level>,
    positioned: bool,
    after_end: bool,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8], info: &RootInfo) -> Self {
        let level = usize::from(info.level);
        Self {
            data,
            block_size: info.block_size as usize,
            root: info.root,
            level,
            empty: info.root_is_fake,
            levels: vec![
                Level {
                    block: 0,
                    c: DIR_START
                };
                level + 1
            ],
            positioned: false,
            after_end: info.root_is_fake,
        }
    }

    pub fn after_end(&self) -> bool {
        self.after_end || !self.positioned
    }

    fn block(&self, n: u32, expected_level: usize) -> Result<&'a [u8]> {
        let start = (n as usize)
            .checked_mul(self.block_size)
            .ok_or_else(|| Error::corrupt("block number overflow"))?;
        let b = self
            .data
            .get(start..start + self.block_size)
            .ok_or_else(|| Error::corrupt(format!("block {n} is outside the database")))?;
        if usize::from(b[4]) != expected_level {
            return Err(Error::corrupt(format!(
                "block {n} has level {} but {expected_level} was expected",
                b[4]
            )));
        }
        let dir_end = read2(b, 9)?;
        if dir_end < DIR_START
            || dir_end > self.block_size
            || !(dir_end - DIR_START).is_multiple_of(D2)
        {
            return Err(Error::corrupt(format!("block {n} has a bad directory")));
        }
        Ok(b)
    }

    fn dir_end(b: &[u8]) -> usize {
        // validated in `block()`
        usize::from(u16::from_be_bytes([b[9], b[10]]))
    }

    fn leaf(&self) -> Result<&'a [u8]> {
        self.block(self.levels[0].block, 0)
    }

    /// Position on the last entry whose key is `<=` `key` (as component 1); returns
    /// whether the key was found exactly. When every key is greater, the cursor sits on
    /// the first entry and `false` is returned.
    pub fn find_entry(&mut self, key: &[u8]) -> Result<bool> {
        if self.empty {
            self.positioned = false;
            self.after_end = true;
            return Ok(false);
        }
        let (key, truncated) = if key.len() > MAX_KEY_LEN {
            (&key[..MAX_KEY_LEN], true)
        } else {
            (key, false)
        };
        self.after_end = false;
        self.positioned = true;
        self.levels[self.level].block = self.root;
        for j in (1..=self.level).rev() {
            let p = self.block(self.levels[j].block, j)?;
            let c = Self::find_in_branch(p, key)?;
            self.levels[j].c = c;
            self.levels[j - 1].block = branch_item(p, c)?.child;
        }
        let p = self.block(self.levels[0].block, 0)?;
        let (c, exact) = Self::find_in_leaf(p, key)?;
        self.levels[0].c = c;
        if c < DIR_START {
            // before the first item of this leaf: step back to the previous leaf's last
            // item, or stay on the very first item of the table
            self.levels[0].c = DIR_START;
            let _ = self.prev_default(0)?;
        }
        Ok(exact && !truncated)
    }

    fn find_in_branch(p: &[u8], key: &[u8]) -> Result<usize> {
        let mut i = DIR_START;
        let mut j = Self::dir_end(p);
        while j - i > D2 {
            let k = i + ((j - i) / (D2 * 2)) * D2;
            let item = branch_item(p, k)?;
            match compare(key, 1, item.key, item.component) {
                std::cmp::Ordering::Less => j = k,
                std::cmp::Ordering::Greater => i = k,
                std::cmp::Ordering::Equal => {
                    i = k;
                    break;
                }
            }
        }
        Ok(i)
    }

    fn find_in_leaf(p: &[u8], key: &[u8]) -> Result<(usize, bool)> {
        let mut i = DIR_START - D2;
        let mut j = Self::dir_end(p);
        let mut exact = false;
        while j - i > D2 {
            let k = i + ((j - i) / (D2 * 2)) * D2;
            let item = leaf_item(p, k)?;
            match compare(key, 1, item.key, item.component) {
                std::cmp::Ordering::Less => j = k,
                std::cmp::Ordering::Greater => i = k,
                std::cmp::Ordering::Equal => {
                    i = k;
                    exact = true;
                    break;
                }
            }
        }
        Ok((i, exact))
    }

    /// Advance one item at level `j` (`GlassTable::next_default`).
    fn next_default(&mut self, j: usize) -> Result<bool> {
        let mut p = self.block(self.levels[j].block, j)?;
        let mut c = self.levels[j].c + D2;
        if c >= Self::dir_end(p) {
            if j == self.level || !self.next_default(j + 1)? {
                return Ok(false);
            }
            p = self.block(self.levels[j].block, j)?;
            c = DIR_START;
        }
        self.levels[j].c = c;
        if j > 0 {
            self.levels[j - 1].block = branch_item(p, c)?.child;
        }
        Ok(true)
    }

    /// Step back one item at level `j` (`GlassTable::prev_default`).
    fn prev_default(&mut self, j: usize) -> Result<bool> {
        let mut p = self.block(self.levels[j].block, j)?;
        let mut c = self.levels[j].c;
        if c == DIR_START {
            if j == self.level || !self.prev_default(j + 1)? {
                return Ok(false);
            }
            p = self.block(self.levels[j].block, j)?;
            c = Self::dir_end(p);
        }
        c -= D2;
        self.levels[j].c = c;
        if j > 0 {
            self.levels[j - 1].block = branch_item(p, c)?.child;
        }
        Ok(true)
    }

    /// Move to the next entry (the next item carrying the FIRST-component bit).
    pub fn next_entry(&mut self) -> Result<bool> {
        if self.after_end() {
            return Ok(false);
        }
        loop {
            if !self.next_default(0)? {
                self.after_end = true;
                self.positioned = false;
                return Ok(false);
            }
            if leaf_item(self.leaf()?, self.levels[0].c)?.first() {
                return Ok(true);
            }
        }
    }

    fn current_item(&self) -> Result<LeafItem<'a>> {
        if self.after_end() {
            return Err(Error::corrupt("cursor is not positioned"));
        }
        leaf_item(self.leaf()?, self.levels[0].c)
    }

    /// Key of the current entry.
    pub fn key(&self) -> Result<&'a [u8]> {
        Ok(self.current_item()?.key)
    }

    /// The whole tag of the current entry, joining split components and inflating
    /// compressed ones. The cursor is left on the entry's first component.
    pub fn tag(&mut self) -> Result<Vec<u8>> {
        // back up to the first component (a non-exact find may land on a later one)
        while !self.current_item()?.first() {
            if !self.prev_default(0)? {
                return Err(Error::corrupt("tag has no first component"));
            }
        }
        let saved = self.levels.clone();
        let mut item = self.current_item()?;
        let compressed = item.compressed();
        let mut raw = Vec::new();
        loop {
            raw.extend_from_slice(item.chunk);
            if item.last() {
                break;
            }
            if !self.next_default(0)? {
                return Err(Error::corrupt("unexpected end of table inside a tag"));
            }
            item = self.current_item()?;
        }
        self.levels = saved;
        if !compressed {
            return Ok(raw);
        }
        // raw deflate (Xapian uses windowBits = -15)
        let mut out = Vec::new();
        flate2::read::DeflateDecoder::new(&raw[..])
            .read_to_end(&mut out)
            .map_err(|e| Error::Inflate(e.to_string()))?;
        Ok(out)
    }

    /// `(key, tag)` when the exact key exists.
    pub fn get_exact(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        if self.find_entry(key)? {
            Ok(Some(self.tag()?))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_comparison() {
        use std::cmp::Ordering::*;
        assert_eq!(compare(b"abc", 1, b"abc", 1), Equal);
        assert_eq!(compare(b"ab", 1, b"abc", 1), Less, "prefix sorts first");
        assert_eq!(compare(b"abd", 1, b"abc", 1), Greater);
        assert_eq!(compare(b"abc", 1, b"abc", 2), Less, "components break ties");
        assert_eq!(compare(b"", 1, b"a", 1), Less);
        assert_eq!(compare(b"\xff", 1, b"a", 1), Greater, "unsigned bytes");
    }

    #[test]
    fn item_parsing_and_bounds() {
        // one leaf block of 64 bytes with a single item "key" -> "tag"
        let mut b = vec![0u8; 64];
        b[4] = 0; // level
        let item = {
            let mut v = Vec::new();
            let size = I2 + K1 + 3 + 3; // header + klen + "key" + "tag"
            let i = (size as u16 - 3) | ((I_FIRST_BIT | I_LAST_BIT) as u16) << 8;
            v.extend_from_slice(&i.to_be_bytes());
            v.push(3);
            v.extend_from_slice(b"key");
            v.extend_from_slice(b"tag");
            v
        };
        let off = 64 - item.len();
        b[off..].copy_from_slice(&item);
        b[9..11].copy_from_slice(&(DIR_START as u16 + 2).to_be_bytes());
        b[DIR_START..DIR_START + 2].copy_from_slice(&(off as u16).to_be_bytes());
        let it = leaf_item(&b, DIR_START).unwrap();
        assert_eq!(it.key, b"key");
        assert_eq!(it.chunk, b"tag");
        assert!(it.first() && it.last() && !it.compressed());
        assert_eq!(it.component, 1);
        // directory pointing outside the block
        b[DIR_START..DIR_START + 2].copy_from_slice(&200u16.to_be_bytes());
        assert!(leaf_item(&b, DIR_START).is_err());
        assert!(leaf_item(&b, 62).is_err());
    }
}
