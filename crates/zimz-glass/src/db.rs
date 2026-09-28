// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! [`GlassDb`]: an opened single-file glass database.

use std::collections::BTreeMap;

use crate::pack::{Reader, push_uint, push_uint_preserving_sort};
use crate::postlist::{PostList, make_key, parse_key};
use crate::table::Cursor;
use crate::version::{TableKind, VersionBlock};
use crate::{Error, Result};

/// A read-only glass database over a byte slice (typically a zero-copy slice of a
/// memory-mapped ZIM at the index item's offset).
#[derive(Debug, Clone)]
pub struct GlassDb<'a> {
    data: &'a [u8],
    version: VersionBlock,
}

/// Statistics of a value slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueStats {
    pub freq: u32,
    pub lower: Vec<u8>,
    pub upper: Vec<u8>,
}

/// One term of the POSTLIST table with its statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermEntry {
    pub term: Vec<u8>,
    pub term_freq: u32,
    pub coll_freq: u64,
}

impl<'a> GlassDb<'a> {
    pub fn open(data: &'a [u8]) -> Result<Self> {
        let version = VersionBlock::parse(data)?;
        if data.len() < version.block_size() as usize {
            return Err(Error::corrupt("database is shorter than one block"));
        }
        Ok(Self { data, version })
    }

    pub fn version(&self) -> &VersionBlock {
        &self.version
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn doc_count(&self) -> u32 {
        self.version.stats.doc_count
    }

    pub fn last_docid(&self) -> u32 {
        self.version.stats.last_docid
    }

    pub fn total_doclen(&self) -> u64 {
        self.version.stats.total_doclen
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn avg_doclen(&self) -> f64 {
        if self.doc_count() == 0 {
            0.0
        } else {
            self.total_doclen() as f64 / f64::from(self.doc_count())
        }
    }

    /// Whether the POSITION table exists (title indexes only; fulltext indexes are
    /// written without positions).
    pub fn has_positions(&self) -> bool {
        !self.version.root(TableKind::Position).root_is_fake
    }

    pub fn has_termlist(&self) -> bool {
        !self.version.root(TableKind::TermList).root_is_fake
    }

    pub fn cursor(&self, kind: TableKind) -> Cursor<'a> {
        Cursor::new(self.data, self.version.root(kind))
    }

    /// User metadata (`\0\xc0` + key in the POSTLIST table).
    pub fn metadata(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let mut k = b"\x00\xc0".to_vec();
        k.extend_from_slice(key.as_bytes());
        self.cursor(TableKind::PostList).get_exact(&k)
    }

    pub fn metadata_string(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .metadata(key)?
            .map(|v| String::from_utf8_lossy(&v).into_owned()))
    }

    /// libzim's `valuesmap` metadata (`title:0;wordcount:1;geo.position:2`).
    pub fn valuesmap(&self) -> Result<BTreeMap<String, u32>> {
        let mut map = BTreeMap::new();
        if let Some(s) = self.metadata_string("valuesmap")? {
            for pair in s.split(';') {
                if let Some((name, slot)) = pair.split_once(':')
                    && let Ok(slot) = slot.trim().parse::<u32>()
                {
                    map.insert(name.trim().to_string(), slot);
                }
            }
        }
        Ok(map)
    }

    pub fn value_slot(&self, name: &str) -> Result<Option<u32>> {
        Ok(self.valuesmap()?.get(name).copied())
    }

    /// The posting list of a term, if the term is indexed.
    pub fn postlist(&self, term: &str) -> Result<Option<PostList<'a>>> {
        self.postlist_bytes(term.as_bytes())
    }

    pub fn postlist_bytes(&self, term: &[u8]) -> Result<Option<PostList<'a>>> {
        if term.is_empty() {
            return Err(Error::corrupt(
                "empty term (use doc_lengths for the length list)",
            ));
        }
        PostList::open(self.cursor(TableKind::PostList), term)
    }

    /// The document-length list (a posting list whose `wdf` is the document length).
    pub fn doc_lengths(&self) -> Result<Option<PostList<'a>>> {
        PostList::open(self.cursor(TableKind::PostList), b"")
    }

    pub fn doc_length(&self, did: u32) -> Result<Option<u32>> {
        let Some(mut pl) = self.doc_lengths()? else {
            return Ok(None);
        };
        Ok(if pl.jump_to(did)? {
            Some(pl.wdf())
        } else {
            None
        })
    }

    /// `(term frequency, collection frequency)` of a term, or `None` if absent.
    pub fn term_freqs(&self, term: &str) -> Result<Option<(u32, u64)>> {
        Ok(self
            .postlist(term)?
            .map(|pl| (pl.term_freq(), pl.coll_freq())))
    }

    /// Document data (libzim stores the entry's full path, e.g. `C/Foo`).
    pub fn docdata(&self, did: u32) -> Result<Option<Vec<u8>>> {
        let mut key = Vec::with_capacity(8);
        push_uint_preserving_sort(&mut key, u64::from(did));
        self.cursor(TableKind::DocData).get_exact(&key)
    }

    pub fn docdata_string(&self, did: u32) -> Result<Option<String>> {
        Ok(self
            .docdata(did)?
            .map(|v| String::from_utf8_lossy(&v).into_owned()))
    }

    /// A document's value in `slot` (stored in value chunks of the POSTLIST table).
    pub fn value(&self, did: u32, slot: u32) -> Result<Option<Vec<u8>>> {
        let mut key = b"\x00\xd8".to_vec();
        push_uint(&mut key, u64::from(slot));
        push_uint_preserving_sort(&mut key, u64::from(did));
        let mut cursor = self.cursor(TableKind::PostList);
        let exact = cursor.find_entry(&key)?;
        if cursor.after_end() {
            return Ok(None);
        }
        let first_did = if exact {
            did
        } else {
            let k = cursor.key()?;
            if k.len() < 2 || k[0] != 0 || k[1] != 0xd8 {
                return Ok(None);
            }
            let mut r = Reader { data: k, pos: 2 };
            if r.u32()? != slot {
                return Ok(None);
            }
            let fd = u32::try_from(r.uint_preserving_sort()?)
                .map_err(|_| Error::corrupt("bad value chunk key"))?;
            if !r.at_end() {
                return Err(Error::corrupt("bad value chunk key"));
            }
            fd
        };
        let chunk = cursor.tag()?;
        let mut r = Reader::new(&chunk);
        let mut cur = first_did;
        let mut value = r.string()?;
        while cur < did {
            if r.at_end() {
                return Ok(None);
            }
            let delta = r.u32()?;
            cur = cur
                .checked_add(delta)
                .and_then(|d| d.checked_add(1))
                .ok_or_else(|| Error::corrupt("docid overflow in value chunk"))?;
            value = r.string()?;
        }
        Ok(if cur == did {
            Some(value.to_vec())
        } else {
            None
        })
    }

    pub fn value_string(&self, did: u32, slot: u32) -> Result<Option<String>> {
        Ok(self
            .value(did, slot)?
            .map(|v| String::from_utf8_lossy(&v).into_owned()))
    }

    /// Frequency and bounds of a value slot.
    pub fn value_stats(&self, slot: u32) -> Result<Option<ValueStats>> {
        let mut key = b"\x00\xd0".to_vec();
        let mut v = u64::from(slot);
        while v != 0 {
            key.push(v as u8);
            v >>= 8;
        }
        let Some(tag) = self.cursor(TableKind::PostList).get_exact(&key)? else {
            return Ok(None);
        };
        let mut r = Reader::new(&tag);
        let freq = r.u32()?;
        let lower = r.string()?.to_vec();
        let upper = if r.at_end() {
            lower.clone()
        } else {
            r.remaining().to_vec()
        };
        Ok(Some(ValueStats { freq, lower, upper }))
    }

    /// All indexed terms starting with `prefix`, in byte order, with their statistics.
    pub fn terms(&self, prefix: &[u8]) -> TermIter<'a> {
        TermIter {
            cursor: self.cursor(TableKind::PostList),
            prefix: prefix.to_vec(),
            state: IterState::Start,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IterState {
    Start,
    Positioned,
    Done,
}

/// Iterator returned by [`GlassDb::terms`].
#[derive(Debug, Clone)]
pub struct TermIter<'a> {
    cursor: Cursor<'a>,
    prefix: Vec<u8>,
    state: IterState,
}

impl TermIter<'_> {
    fn step(&mut self) -> Result<Option<TermEntry>> {
        loop {
            match self.state {
                IterState::Done => return Ok(None),
                IterState::Start => {
                    self.state = IterState::Positioned;
                    // any term key is > "\0\xff…" (special keys start with \0); a prefix
                    // key sorts before every key that extends it
                    let start = if self.prefix.is_empty() {
                        b"\x00\xff".to_vec()
                    } else {
                        make_key(&self.prefix, None)
                    };
                    self.cursor.find_entry(&start)?;
                    if self.cursor.after_end() {
                        self.state = IterState::Done;
                        return Ok(None);
                    }
                    if self.cursor.key()? < start.as_slice() && !self.cursor.next_entry()? {
                        self.state = IterState::Done;
                        return Ok(None);
                    }
                }
                IterState::Positioned => {
                    if self.cursor.after_end() {
                        self.state = IterState::Done;
                        return Ok(None);
                    }
                    let key = self.cursor.key()?;
                    match parse_key(key)? {
                        Some((term, None)) if !term.is_empty() => {
                            if !term.starts_with(&self.prefix) {
                                self.state = IterState::Done;
                                return Ok(None);
                            }
                            let tag = self.cursor.tag()?;
                            let mut r = Reader::new(&tag);
                            let term_freq = r.u32()?;
                            let coll_freq = r.uint()?;
                            if !self.cursor.next_entry()? {
                                self.state = IterState::Done;
                            }
                            return Ok(Some(TermEntry {
                                term,
                                term_freq,
                                coll_freq,
                            }));
                        }
                        _ => {
                            // special key, doclen list, or a continuation chunk
                            if !self.cursor.next_entry()? {
                                self.state = IterState::Done;
                                return Ok(None);
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Iterator for TermIter<'_> {
    type Item = Result<TermEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.step() {
            Ok(Some(e)) => Some(Ok(e)),
            Ok(None) => None,
            Err(e) => {
                self.state = IterState::Done;
                Some(Err(e))
            }
        }
    }
}
