// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Position lists (`backends/glass/glass_positionlist.cc`, `common/bitstream.cc`):
//! per `(term, docid)`, the term positions encoded as the last position, then an
//! interpolative bit-stream of the rest. Only title indexes carry them.

use crate::pack::{Reader, push_string_preserving_sort, push_uint_preserving_sort};
use crate::{Error, Result};

/// POSITION table key: sort-packed term (with terminator) + sort-packed docid.
pub fn make_key(term: &[u8], did: u32) -> Vec<u8> {
    let mut key = Vec::with_capacity(term.len() + 8);
    push_string_preserving_sort(&mut key, term, false);
    push_uint_preserving_sort(&mut key, u64::from(did));
    key
}

/// LSB-first bit reader (`BitReader`).
struct BitReader<'a> {
    buf: &'a [u8],
    idx: usize,
    acc: u64,
    n_bits: u32,
}

fn highest_order_bit(mask: u64) -> u32 {
    64 - mask.leading_zeros()
}

impl BitReader<'_> {
    fn read_bits(&mut self, count: u32) -> Result<u64> {
        if count > 57 {
            let lo = self.read_bits(32)?;
            return Ok(lo | (self.read_bits(count - 32)? << 32));
        }
        while self.n_bits < count {
            let byte = *self
                .buf
                .get(self.idx)
                .ok_or_else(|| Error::corrupt("position list bit-stream truncated"))?;
            self.idx += 1;
            self.acc |= u64::from(byte) << self.n_bits;
            self.n_bits += 8;
        }
        let result = self.acc & ((1u64 << count) - 1);
        self.acc >>= count;
        self.n_bits -= count;
        Ok(result)
    }

    /// Decode a value in `0..outof` (`BitReader::decode`).
    fn decode(&mut self, outof: u64) -> Result<u64> {
        let bits = highest_order_bit(outof.wrapping_sub(1));
        let spare = (if bits >= 64 { 0 } else { 1u64 << bits }).wrapping_sub(outof);
        let mid_start = (outof.wrapping_sub(spare)) / 2;
        if spare != 0 {
            let mut p = self.read_bits(bits - 1)?;
            if p < mid_start && self.read_bits(1)? != 0 {
                p += mid_start + spare;
            }
            Ok(p)
        } else {
            self.read_bits(bits)
        }
    }

    /// Fill `pos[j+1..k]` given `pos[j]` and `pos[k]`, mirroring `encode_interpolative`.
    fn decode_interpolative(&mut self, pos: &mut [u64], mut j: usize, k: usize) -> Result<()> {
        while j + 1 < k {
            let mid = j + (k - j) / 2;
            let span = pos[k]
                .checked_sub(pos[j])
                .ok_or_else(|| Error::corrupt("positions not increasing"))?;
            let outof = span
                .checked_add(1)
                .and_then(|v| v.checked_sub((k - j) as u64))
                .ok_or_else(|| Error::corrupt("bad position range"))?;
            let lowest = pos[j] + (mid - j) as u64;
            pos[mid] = self.decode(outof)? + lowest;
            self.decode_interpolative(pos, j, mid)?;
            j = mid;
        }
        Ok(())
    }
}

/// Decode a position-list tag into ascending positions.
pub fn decode_positions(data: &[u8]) -> Result<Vec<u32>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let mut r = Reader::new(data);
    let last = r.uint()?;
    if r.at_end() {
        return Ok(vec![
            u32::try_from(last).map_err(|_| Error::corrupt("position overflow"))?,
        ]);
    }
    let mut rd = BitReader {
        buf: data,
        idx: r.pos,
        acc: 0,
        n_bits: 0,
    };
    let first = rd.decode(last)?;
    let size = rd
        .decode(last - first)?
        .checked_add(2)
        .ok_or_else(|| Error::corrupt("position count overflow"))?;
    if size > 1 << 24 {
        return Err(Error::corrupt("implausible position count"));
    }
    let size = size as usize;
    let mut pos = vec![0u64; size];
    pos[0] = first;
    pos[size - 1] = last;
    rd.decode_interpolative(&mut pos, 0, size - 1)?;
    pos.into_iter()
        .map(|p| u32::try_from(p).map_err(|_| Error::corrupt("position overflow")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of Xapian's `BitWriter` + `pack`, only for tests.
    struct BitWriter {
        buf: Vec<u8>,
        acc: u64,
        n_bits: u32,
    }

    impl BitWriter {
        fn encode(&mut self, mut value: u64, outof: u64) {
            let mut bits = highest_order_bit(outof.wrapping_sub(1));
            let spare = (if bits >= 64 { 0 } else { 1u64 << bits }).wrapping_sub(outof);
            if spare != 0 {
                let mid_start = (outof - spare) / 2;
                if value >= mid_start + spare {
                    value = (value - (mid_start + spare)) | (1u64 << (bits - 1));
                } else if value >= mid_start {
                    bits -= 1;
                }
            }
            if bits + self.n_bits > 64 {
                self.acc |= value << self.n_bits;
                self.buf.push(self.acc as u8);
                self.acc >>= 8;
                value >>= 8;
                bits -= 8;
            }
            self.acc |= value << self.n_bits;
            self.n_bits += bits;
            while self.n_bits >= 8 {
                self.buf.push(self.acc as u8);
                self.acc >>= 8;
                self.n_bits -= 8;
            }
        }

        fn encode_interpolative(&mut self, pos: &[u64], mut j: usize, k: usize) {
            while j + 1 < k {
                let mid = j + (k - j) / 2;
                let outof = pos[k] - pos[j] + 1 - (k - j) as u64;
                let lowest = pos[j] + (mid - j) as u64;
                self.encode(pos[mid] - lowest, outof);
                self.encode_interpolative(pos, j, mid);
                j = mid;
            }
        }

        fn freeze(mut self) -> Vec<u8> {
            if self.n_bits > 0 {
                self.buf.push(self.acc as u8);
            }
            self.buf
        }
    }

    fn pack(positions: &[u64]) -> Vec<u8> {
        let mut s = Vec::new();
        crate::pack::push_uint(&mut s, *positions.last().unwrap());
        if positions.len() > 1 {
            let mut w = BitWriter {
                buf: s,
                acc: 0,
                n_bits: 0,
            };
            w.encode(positions[0], *positions.last().unwrap());
            w.encode(
                positions.len() as u64 - 2,
                positions.last().unwrap() - positions[0],
            );
            w.encode_interpolative(positions, 0, positions.len() - 1);
            return w.freeze();
        }
        s
    }

    #[test]
    fn roundtrip_various_lists() {
        let cases: Vec<Vec<u64>> = vec![
            vec![1],
            vec![7],
            vec![1, 2],
            vec![1, 2, 3],
            vec![3, 9, 27, 81],
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            vec![2, 5, 6, 40, 41, 42, 1000, 1001, 5000],
            (1..300).map(|i| i * 4 + (i % 3)).collect(),
            vec![100_000, 100_001],
        ];
        for c in cases {
            let packed = pack(&c);
            let decoded: Vec<u64> = decode_positions(&packed)
                .unwrap()
                .into_iter()
                .map(u64::from)
                .collect();
            assert_eq!(decoded, c, "packed {packed:?}");
        }
        assert!(decode_positions(b"").unwrap().is_empty());
        assert!(decode_positions(&[0x85]).is_err(), "truncated varint");
    }

    #[test]
    fn keys() {
        assert_eq!(make_key(b"term", 5), b"term\x00\x00\x05");
        assert_eq!(make_key(b"a\0b", 1), b"a\x00\xffb\x00\x00\x01");
    }
}
