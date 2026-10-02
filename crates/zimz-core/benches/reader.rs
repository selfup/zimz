// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Criterion micro-benchmarks for the reader hot paths. Self-contained: the archive is
//! built in memory by the shared test builder, so `cargo bench` needs no fixtures.
//!
//! Run: `cargo bench -p zimz-core --bench reader`
//!
//! These measure regressions against the fixed targets in `docs/P1-core-reader.md`; the
//! absolute numbers there come from real archives via `scripts/bench.sh`.

#[path = "../tests/common/builder.rs"]
#[allow(dead_code)]
mod builder;

use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use zimz_core::Archive;
use zimz_core::source::MemorySource;

use builder::{Codec, ZimBuilder};

const ENTRIES: usize = 4000;

/// A deterministic pseudo-random sequence of entry indexes in `0..n`.
fn indexes(n: u32, count: usize) -> Vec<u32> {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    (0..count)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % u64::from(n)) as u32
        })
        .collect()
}

/// `n` front articles of ~1 KiB each, `codec`-compressed, in 64 KiB clusters.
fn wiki(n: usize, codec: Codec) -> Vec<u8> {
    let mut b = ZimBuilder::new_scheme();
    b.codec = codec;
    let body = "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(20);
    for i in 0..n {
        b = b.html(&format!("Article_{i:06}"), &format!("Article {i}"), &body);
    }
    b.build()
}

fn open(bytes: Vec<u8>) -> Archive {
    Archive::from_source(Box::new(MemorySource::new(bytes))).unwrap()
}

/// Open over the same bytes; a clone (untimed setup) gives every iteration an empty
/// cluster cache, which is what "cold" means here.
fn fresh(bytes: &[u8]) -> Archive {
    open(bytes.to_vec())
}

fn bench_open(c: &mut Criterion) {
    let bytes = wiki(ENTRIES, Codec::None);
    c.bench_with_input(BenchmarkId::new("open", ENTRIES), &bytes, |b, bytes| {
        b.iter_batched(
            || bytes.clone(),
            |data| black_box(open(data).entry_count()),
            BatchSize::SmallInput,
        );
    });
}

fn bench_lookup(c: &mut Criterion) {
    let bytes = wiki(ENTRIES, Codec::None);
    let archive = open(bytes);
    let n = archive.entry_count();
    let idx = indexes(n, 2048);

    let mut group = c.benchmark_group("lookup");
    group.bench_function("entry_by_index", |b| {
        b.iter(|| {
            for &i in &idx {
                black_box(archive.entry(i).unwrap());
            }
        });
    });
    group.bench_function("entry_by_path", |b| {
        let paths: Vec<String> = idx
            .iter()
            .map(|&i| archive.entry(i).unwrap().path.clone())
            .collect();
        b.iter(|| {
            for p in &paths {
                black_box(archive.entry_by_path(b'C', p).unwrap());
            }
        });
    });
    group.bench_function("title_prefix", |b| {
        b.iter(|| {
            for i in 0..2048u32 {
                let prefix = format!("Article {i:04}");
                black_box(archive.find_title_prefix(b'C', &prefix).unwrap());
            }
        });
    });
    group.finish();
}

fn bench_item_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("item_read");

    let mut codecs = vec![("none", Codec::None)];
    #[cfg(feature = "zstd-c")]
    codecs.push(("zstd", Codec::Zstd));

    // Warm: one archive, every cluster decoded once before the timed loop.
    for (name, codec) in &codecs {
        let label = format!("cached/{name}");
        let archive = open(wiki(ENTRIES, *codec));
        let items: Vec<_> = (0..ENTRIES as u32)
            .step_by(ENTRIES / 512)
            .map(|i| archive.entry(i).unwrap())
            .collect();
        for d in &items {
            let _ = archive.item_data(d).unwrap();
        }
        group.bench_function(label, |b| {
            b.iter(|| {
                for d in &items {
                    black_box(archive.item_data(d).unwrap().len());
                }
            });
        });
    }

    // Cold: a fresh archive per iteration decodes exactly one cluster per item read.
    for (name, codec) in &codecs {
        let label = format!("cold/{name}");
        let bytes = wiki(ENTRIES, *codec);
        let probes = indexes(ENTRIES as u32, 32);
        group.bench_function(label, |b| {
            b.iter_batched(
                || fresh(&bytes),
                |a| {
                    for &i in &probes {
                        let d = a.entry(i).unwrap();
                        black_box(a.item_data(&d).unwrap().len());
                    }
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_open, bench_lookup, bench_item_read);
criterion_main!(benches);
