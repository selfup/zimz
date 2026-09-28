// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Xapian's integer and string encodings (`common/pack.h`).

use crate::{Error, Result};

/// A cursor over a byte slice for the `unpack_*` functions.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.pos.min(self.data.len())..]
    }

    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or_else(|| Error::corrupt("unexpected end of encoded data"))?;
        self.pos += 1;
        Ok(b)
    }

    /// LEB128: 7 bits per byte, least significant first, high bit = continue.
    pub fn uint(&mut self) -> Result<u64> {
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let b = self.byte()?;
            if shift >= 64 || (shift > 56 && (u64::from(b & 0x7f) >> (64 - shift)) != 0) {
                return Err(Error::corrupt("varint overflows 64 bits"));
            }
            result |= u64::from(b & 0x7f) << shift;
            if b < 0x80 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    pub fn u32(&mut self) -> Result<u32> {
        u32::try_from(self.uint()?).map_err(|_| Error::corrupt("varint overflows 32 bits"))
    }

    /// `'0'` / `'1'`.
    pub fn bool(&mut self) -> Result<bool> {
        match self.byte()? {
            b'0' => Ok(false),
            b'1' => Ok(true),
            _ => Err(Error::corrupt("bad packed bool")),
        }
    }

    /// Sort-preserving integer: `< 0x8000` as two big-endian bytes, otherwise a length
    /// prefix in leading one-bits.
    pub fn uint_preserving_sort(&mut self) -> Result<u64> {
        let len_byte = self.byte()?;
        if len_byte < 0x80 {
            return Ok(u64::from(len_byte) << 8 | u64::from(self.byte()?));
        }
        if len_byte == 0xff {
            return Err(Error::corrupt("bad sort-preserving integer"));
        }
        let mut len = 2usize;
        let mut m = 0x40u8;
        while len_byte & m != 0 {
            len += 1;
            m >>= 1;
        }
        if len > 8 {
            return Err(Error::corrupt("sort-preserving integer too wide"));
        }
        let mask = 0xffu32 << (9 - len);
        let mut r = u64::from(len_byte) & !u64::from(mask & 0xff);
        for _ in 0..len {
            r = r << 8 | u64::from(self.byte()?);
        }
        Ok(r)
    }

    /// `pack_uint(len)` followed by the bytes.
    pub fn string(&mut self) -> Result<&'a [u8]> {
        let len =
            usize::try_from(self.uint()?).map_err(|_| Error::corrupt("string length overflow"))?;
        let end = self
            .pos
            .checked_add(len)
            .filter(|e| *e <= self.data.len())
            .ok_or_else(|| Error::corrupt("string runs past the end"))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// Minimal little-endian bytes to the end of the data (`pack_uint_last`).
    pub fn uint_last(&mut self) -> Result<u64> {
        let rest = self.remaining();
        if rest.len() > 8 {
            return Err(Error::corrupt("uint_last too wide"));
        }
        let mut r = 0u64;
        for &b in rest.iter().rev() {
            r = r << 8 | u64::from(b);
        }
        self.pos = self.data.len();
        Ok(r)
    }
}

/// Sort-preserving encoding of an unsigned integer (`pack_uint_preserving_sort`).
pub fn push_uint_preserving_sort(out: &mut Vec<u8>, value: u64) {
    if value < 0x8000 {
        out.push((value >> 8) as u8);
        out.push(value as u8);
        return;
    }
    let mut len = 3usize;
    let mut x = value >> 22;
    while x != 0 {
        len += 1;
        x >>= 7;
    }
    let mask = (0xffu32 << (10 - len)) as u8;
    let start = out.len();
    out.resize(start + len, 0);
    let mut v = value;
    for i in 1..len {
        out[start + len - i] = v as u8;
        v >>= 8;
    }
    out[start] = (v as u8) | mask;
}

/// LEB128 encoding (`pack_uint`).
pub fn push_uint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// `pack_string_preserving_sort`: `\0` becomes `\0\xff`, then a `\0` terminator
/// unless `last`.
pub fn push_string_preserving_sort(out: &mut Vec<u8>, value: &[u8], last: bool) {
    for &b in value {
        out.push(b);
        if b == 0 {
            out.push(0xff);
        }
    }
    if !last {
        out.push(0);
    }
}

/// Inverse of [`push_string_preserving_sort`]: reads up to the terminator (consumed) or
/// the end of the data. Returns the string and whether a terminator was found.
pub fn read_string_preserving_sort(r: &mut Reader<'_>) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    while let Some(&b) = r.data.get(r.pos) {
        r.pos += 1;
        if b == 0 {
            if r.data.get(r.pos) == Some(&0xff) {
                r.pos += 1;
                out.push(0);
                continue;
            }
            return (out, true);
        }
        out.push(b);
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_roundtrip() {
        for v in [
            0u64,
            1,
            127,
            128,
            300,
            16383,
            16384,
            u32::MAX as u64,
            u64::MAX >> 1,
            u64::MAX,
        ] {
            let mut buf = Vec::new();
            push_uint(&mut buf, v);
            let mut r = Reader::new(&buf);
            assert_eq!(r.uint().unwrap(), v);
            assert!(r.at_end());
        }
        assert!(Reader::new(&[0x80]).uint().is_err(), "truncated");
        assert!(Reader::new(&[0xff; 11]).uint().is_err(), "overflow");
    }

    #[test]
    fn sort_preserving_roundtrip_and_order() {
        let values = [
            0u64,
            1,
            255,
            256,
            0x7fff,
            0x8000,
            0x8001,
            0x3f_ffff,
            0x40_0000,
            0x1fff_ffff,
            0x2000_0000,
            u32::MAX as u64,
            1 << 40,
            u64::MAX >> 8,
        ];
        let mut prev: Option<Vec<u8>> = None;
        for &v in &values {
            let mut buf = Vec::new();
            push_uint_preserving_sort(&mut buf, v);
            let mut r = Reader::new(&buf);
            assert_eq!(r.uint_preserving_sort().unwrap(), v, "{v:#x}");
            assert!(r.at_end());
            if let Some(p) = &prev {
                assert!(p < &buf, "encoding must preserve order at {v:#x}");
            }
            prev = Some(buf);
        }
        assert!(Reader::new(&[0xff, 0]).uint_preserving_sort().is_err());
        assert!(
            Reader::new(&[0x12]).uint_preserving_sort().is_err(),
            "truncated"
        );
    }

    #[test]
    fn strings_and_bools() {
        let mut buf = Vec::new();
        push_uint(&mut buf, 3);
        buf.extend_from_slice(b"abcXYZ");
        let mut r = Reader::new(&buf);
        assert_eq!(r.string().unwrap(), b"abc");
        assert_eq!(r.remaining(), b"XYZ");
        let mut r = Reader::new(b"01x");
        assert!(!r.bool().unwrap());
        assert!(r.bool().unwrap());
        assert!(r.bool().is_err());
        let mut k = Vec::new();
        push_string_preserving_sort(&mut k, b"a\0b", false);
        assert_eq!(k, b"a\0\xffb\0");
        let mut r = Reader::new(&k);
        let (s, terminated) = read_string_preserving_sort(&mut r);
        assert_eq!((s.as_slice(), terminated), (&b"a\0b"[..], true));
        let mut k = Vec::new();
        push_string_preserving_sort(&mut k, b"term", true);
        let (s, terminated) = read_string_preserving_sort(&mut Reader::new(&k));
        assert_eq!((s.as_slice(), terminated), (&b"term"[..], false));
        let mut r = Reader::new(&[0x34, 0x12]);
        assert_eq!(r.uint_last().unwrap(), 0x1234);
    }
}
