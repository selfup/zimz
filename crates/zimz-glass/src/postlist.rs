// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Posting lists (`backends/glass/glass_postlist.cc`): chunked `(docid, wdf)` lists
//! stored in the POSTLIST table. The document-length list is the posting list of the
//! empty term (key `\0\xe0`).

use crate::pack::{
    Reader, push_string_preserving_sort, push_uint_preserving_sort, read_string_preserving_sort,
};
use crate::table::Cursor;
use crate::{Error, Result};

/// Key of a term's first chunk, or of the chunk starting at `did`.
pub fn make_key(term: &[u8], did: Option<u32>) -> Vec<u8> {
    let mut key = Vec::with_capacity(term.len() + 8);
    if term.is_empty() {
        key.extend_from_slice(b"\x00\xe0");
    } else {
        push_string_preserving_sort(&mut key, term, did.is_none());
    }
    if let Some(did) = did {
        push_uint_preserving_sort(&mut key, u64::from(did));
    }
    key
}

/// Split a POSTLIST key into `(term, first docid of the chunk)`; `None` when the key is
/// not a posting-list key (metadata, value chunks, …).
pub fn parse_key(key: &[u8]) -> Result<Option<(Vec<u8>, Option<u32>)>> {
    let mut r = Reader::new(key);
    let term = if key.starts_with(b"\x00\xe0") {
        r.pos = 2;
        Vec::new()
    } else if key.first() == Some(&0) {
        return Ok(None);
    } else {
        let (term, terminated) = read_string_preserving_sort(&mut r);
        if !terminated {
            return Ok(Some((term, None)));
        }
        term
    };
    if r.at_end() {
        return Ok(Some((term, None)));
    }
    let did = u32::try_from(r.uint_preserving_sort()?)
        .map_err(|_| Error::corrupt("docid overflow in postlist key"))?;
    Ok(Some((term, Some(did))))
}

/// Iterator over the `(docid, wdf)` pairs of one term.
#[derive(Debug, Clone)]
pub struct PostList<'a> {
    cursor: Cursor<'a>,
    term: Vec<u8>,
    tag: Vec<u8>,
    pos: usize,
    did: u32,
    wdf: u32,
    first_did_in_chunk: u32,
    last_did_in_chunk: u32,
    is_last_chunk: bool,
    at_end: bool,
    term_freq: u32,
    coll_freq: u64,
    wdf_upper_bound: u32,
}

impl<'a> PostList<'a> {
    /// Open the posting list of `term` (empty = document lengths); `None` if absent.
    pub(crate) fn open(mut cursor: Cursor<'a>, term: &[u8]) -> Result<Option<Self>> {
        if !cursor.find_entry(&make_key(term, None))? {
            return Ok(None);
        }
        let tag = cursor.tag()?;
        let mut pl = Self {
            cursor,
            term: term.to_vec(),
            tag,
            pos: 0,
            did: 0,
            wdf: 0,
            first_did_in_chunk: 0,
            last_did_in_chunk: 0,
            is_last_chunk: true,
            at_end: false,
            term_freq: 0,
            coll_freq: 0,
            wdf_upper_bound: 0,
        };
        pl.read_first_chunk_header()?;
        pl.wdf_upper_bound = u32::try_from(
            pl.coll_freq
                .saturating_sub(u64::from(pl.wdf))
                .max(u64::from(pl.wdf)),
        )
        .unwrap_or(u32::MAX);
        Ok(Some(pl))
    }

    /// First chunk: entries, collection frequency, first docid, then a chunk header.
    fn read_first_chunk_header(&mut self) -> Result<()> {
        let mut r = Reader {
            data: &self.tag,
            pos: 0,
        };
        self.term_freq = r.u32()?;
        self.coll_freq = r.uint()?;
        let did = r
            .u32()?
            .checked_add(1)
            .ok_or_else(|| Error::corrupt("docid overflow"))?;
        self.pos = r.pos;
        self.did = did;
        self.first_did_in_chunk = did;
        self.read_chunk_header()
    }

    /// `is_last_chunk`, last docid delta, first wdf.
    fn read_chunk_header(&mut self) -> Result<()> {
        let mut r = Reader {
            data: &self.tag,
            pos: self.pos,
        };
        self.is_last_chunk = r.bool()?;
        let to_last = r.u32()?;
        self.last_did_in_chunk = self
            .first_did_in_chunk
            .checked_add(to_last)
            .ok_or_else(|| Error::corrupt("docid overflow"))?;
        self.wdf = r.u32()?;
        self.pos = r.pos;
        Ok(())
    }

    pub fn term(&self) -> &[u8] {
        &self.term
    }

    pub fn term_freq(&self) -> u32 {
        self.term_freq
    }

    pub fn coll_freq(&self) -> u64 {
        self.coll_freq
    }

    pub fn wdf_upper_bound(&self) -> u32 {
        self.wdf_upper_bound
    }

    pub fn at_end(&self) -> bool {
        self.at_end
    }

    /// Current document id (valid unless `at_end`).
    pub fn docid(&self) -> u32 {
        self.did
    }

    /// Within-document frequency of the current posting (document length for the
    /// document-length list).
    pub fn wdf(&self) -> u32 {
        self.wdf
    }

    fn next_in_chunk(&mut self) -> Result<bool> {
        if self.pos >= self.tag.len() {
            return Ok(false);
        }
        let mut r = Reader {
            data: &self.tag,
            pos: self.pos,
        };
        let inc = r.u32()?;
        self.did = self
            .did
            .checked_add(inc)
            .and_then(|d| d.checked_add(1))
            .ok_or_else(|| Error::corrupt("docid overflow in postlist"))?;
        self.wdf = r.u32()?;
        self.pos = r.pos;
        Ok(true)
    }

    fn next_chunk(&mut self) -> Result<()> {
        if self.is_last_chunk {
            self.at_end = true;
            return Ok(());
        }
        if !self.cursor.next_entry()? {
            self.at_end = true;
            return Err(Error::corrupt("unexpected end of posting list"));
        }
        let Some((term, Some(newdid))) = parse_key(self.cursor.key()?)? else {
            self.at_end = true;
            return Err(Error::corrupt("posting list chunk has a bad key"));
        };
        if term != self.term || newdid <= self.did {
            self.at_end = true;
            return Err(Error::corrupt("posting list chunks out of order"));
        }
        self.did = newdid;
        self.tag = self.cursor.tag()?;
        self.pos = 0;
        self.first_did_in_chunk = newdid;
        self.read_chunk_header()
    }

    /// Advance to the next posting; `false` at the end.
    pub fn advance(&mut self) -> Result<bool> {
        if self.at_end {
            return Ok(false);
        }
        if !self.next_in_chunk()? {
            self.next_chunk()?;
        }
        Ok(!self.at_end)
    }

    fn move_forward_in_chunk_to_at_least(&mut self, desired: u32) -> Result<bool> {
        if self.did >= desired {
            return Ok(true);
        }
        if desired <= self.last_did_in_chunk {
            while self.pos < self.tag.len() {
                let mut r = Reader {
                    data: &self.tag,
                    pos: self.pos,
                };
                let inc = r.u32()?;
                self.did = self
                    .did
                    .checked_add(inc)
                    .and_then(|d| d.checked_add(1))
                    .ok_or_else(|| Error::corrupt("docid overflow in postlist"))?;
                self.wdf = r.u32()?;
                self.pos = r.pos;
                if self.did >= desired {
                    return Ok(true);
                }
            }
            return Err(Error::corrupt(
                "posting list chunk ended before its last docid",
            ));
        }
        Ok(false)
    }

    fn move_to_chunk_containing(&mut self, desired: u32) -> Result<()> {
        let _ = self
            .cursor
            .find_entry(&make_key(&self.term, Some(desired)))?;
        let key = if self.cursor.after_end() {
            None
        } else {
            Some(self.cursor.key()?)
        };
        let parsed = match key {
            Some(k) => parse_key(k)?,
            None => None,
        };
        let Some((term, did)) = parsed else {
            self.at_end = true;
            self.is_last_chunk = true;
            return Ok(());
        };
        if term != self.term {
            self.at_end = true;
            self.is_last_chunk = true;
            return Ok(());
        }
        self.at_end = false;
        self.tag = self.cursor.tag()?;
        self.pos = 0;
        match did {
            None => {
                let mut r = Reader {
                    data: &self.tag,
                    pos: 0,
                };
                let _tf = r.uint()?;
                let _cf = r.uint()?;
                self.did = r
                    .u32()?
                    .checked_add(1)
                    .ok_or_else(|| Error::corrupt("docid overflow"))?;
                self.pos = r.pos;
            }
            Some(d) => self.did = d,
        }
        self.first_did_in_chunk = self.did;
        self.read_chunk_header()?;
        if desired > self.last_did_in_chunk {
            self.next_chunk()?;
        }
        Ok(())
    }

    /// Move to the first posting with `docid >= desired`; `false` at the end.
    pub fn skip_to(&mut self, desired: u32) -> Result<bool> {
        if self.at_end {
            return Ok(false);
        }
        if self.did >= desired {
            return Ok(true);
        }
        if desired > self.last_did_in_chunk {
            self.move_to_chunk_containing(desired)?;
            if self.at_end {
                return Ok(false);
            }
        }
        if !self.move_forward_in_chunk_to_at_least(desired)? && !self.at_end {
            // the chunk we moved to starts after `desired`
            return Ok(self.did >= desired);
        }
        Ok(!self.at_end)
    }

    /// Whether `did` is in the list; positions the list on it (or after it) as a side
    /// effect, like Xapian's `jump_to`.
    pub fn jump_to(&mut self, did: u32) -> Result<bool> {
        Ok(self.skip_to(did)? && self.did == did)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(make_key(b"", None), b"\x00\xe0");
        assert_eq!(make_key(b"term", None), b"term");
        assert_eq!(make_key(b"term", Some(5)), b"term\x00\x00\x05");
        assert_eq!(make_key(b"a\0b", None), b"a\x00\xffb");
        assert_eq!(parse_key(b"term").unwrap(), Some((b"term".to_vec(), None)));
        assert_eq!(
            parse_key(b"term\x00\x00\x05").unwrap(),
            Some((b"term".to_vec(), Some(5)))
        );
        assert_eq!(parse_key(b"\x00\xe0").unwrap(), Some((Vec::new(), None)));
        assert_eq!(
            parse_key(b"\x00\xe0\x00\x09").unwrap(),
            Some((Vec::new(), Some(9)))
        );
        assert_eq!(
            parse_key(b"\x00\xc0valuesmap").unwrap(),
            None,
            "metadata keys are not postings"
        );
        assert_eq!(
            parse_key(b"a\x00\xffb").unwrap(),
            Some((b"a\0b".to_vec(), None))
        );
    }
}
