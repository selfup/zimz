// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Corrupt valid archives in thousands of random ways and assert the reader never
//! panics (errors are fine). Runs on stable without cargo-fuzz; `ZIMZ_MUTATIONS=n`
//! raises the iteration count per base archive.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use common::builder::sample;
use common::open_bytes;
use zimz_core::integrity::{self, Check};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn iterations() -> u64 {
    std::env::var("ZIMZ_MUTATIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600)
}

/// Everything a consumer might do with an archive; results are ignored, panics are not.
fn exercise(bytes: Vec<u8>) {
    let Ok(a) = open_bytes(bytes) else { return };
    let _ = a
        .main_entry()
        .and_then(|m| m.map(|m| a.resolve(&m)).transpose());
    for e in a.entries().take(2000).flatten() {
        let _ = a.item_data(&e);
        let _ = a.direct_access(&e);
        let _ = a.mime_type(&e);
        let _ = a.resolve(&e);
    }
    for pos in 0..a.title_index().len().min(500) {
        let _ = a.entry_by_title_position(pos);
    }
    let _ = a.find_title_prefix(a.content_namespace(), "A");
    let _ = a.entry_by_title("Apple");
    let _ = a.entry_by_path_compat("Home");
    let _ = a.namespaces();
    let _ = a.metadata("Title");
    let _ = a.counter();
    let _ = a.illustration(48, 48, 1.0);
    let _ = a.article_count();
    let _ = a.fulltext_index();
    let _ = a.verify_checksum();
    let _ = integrity::run(&a, &Check::ALL);
}

fn mutate(rng: &mut Rng, base: &[u8]) -> (String, Vec<u8>) {
    let mut b = base.to_vec();
    match rng.below(6) {
        0 => {
            let n = 1 + rng.below(8);
            let mut where_ = Vec::new();
            for _ in 0..n {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
                where_.push(i);
            }
            (format!("flip bits at {where_:?}"), b)
        }
        1 => {
            let at = rng.below(b.len());
            b.truncate(at);
            (format!("truncate to {at}"), b)
        }
        2 => {
            let at = rng.below(b.len());
            let len = rng.below(64).min(b.len() - at);
            b[at..at + len].fill(0);
            (format!("zero {len} bytes at {at}"), b)
        }
        3 => {
            let at = rng.below(b.len().saturating_sub(8));
            let v: u64 = [
                0,
                1,
                0x7f,
                0xff,
                u32::MAX as u64,
                u64::MAX,
                base.len() as u64,
                base.len() as u64 - 1,
            ][rng.below(8)];
            let width = if rng.below(2) == 0 { 4 } else { 8 };
            b[at..at + width].copy_from_slice(&v.to_le_bytes()[..width]);
            (format!("set {width}-byte field at {at} to {v:#x}"), b)
        }
        4 => {
            let at = rng.below(b.len());
            let n = 1 + rng.below(32);
            let junk: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            b.splice(at..at, junk);
            (format!("insert {n} bytes at {at}"), b)
        }
        _ => {
            let at = rng.below(b.len());
            let n = (1 + rng.below(32)).min(b.len() - at);
            b.drain(at..at + n);
            (format!("delete {n} bytes at {at}"), b)
        }
    }
}

fn run(name: &str, base: &[u8], seed: u64) {
    let mut rng = Rng(seed | 1);
    for i in 0..iterations() {
        let state = rng.0;
        let (desc, m) = mutate(&mut rng, base);
        let started = std::time::Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| exercise(m.clone())));
        let took = started.elapsed();
        if took.as_millis() > 200 {
            eprintln!("{name}: slow mutation #{i} ({desc}) took {took:.1?}");
        }
        if result.is_err() {
            let out = std::env::temp_dir().join(format!("zimz-mutation-failure-{name}-{i}.zim"));
            let _ = std::fs::write(&out, &m);
            panic!(
                "{name}: PANIC on mutation #{i} ({desc}, rng state {state:#x}); input saved to {}",
                out.display()
            );
        }
    }
}

#[test]
fn corrupted_fixtures_never_panic() {
    let Some(dir) = common::fixtures_dir() else {
        return;
    };
    for flavour in ["withns", "nons", "noTitleListingV0"] {
        let bytes = std::fs::read(dir.join(flavour).join("small.zim")).unwrap();
        run(flavour, &bytes, 0x1234_5678 ^ flavour.len() as u64);
    }
}

#[test]
fn corrupted_synthetic_archives_never_panic() {
    run("new", &sample(true).build(), 42);
    run("old", &sample(false).build(), 43);
    #[cfg(feature = "zstd-c")]
    {
        let mut b = sample(true);
        b.codec = common::builder::Codec::Zstd;
        run("zstd", &b.build(), 44);
        let mut b = common::builder::ZimBuilder::new_scheme();
        b.codec = common::builder::Codec::Zstd;
        b.extended = true;
        b = b.html("A", "A", "a").html("B", "B", "b");
        run("extended", &b.build(), 45);
    }
    #[cfg(feature = "xz-c")]
    {
        let mut b = sample(false);
        b.codec = common::builder::Codec::Xz;
        run("xz", &b.build(), 46);
    }
}

#[test]
fn exhaustive_single_byte_flips_of_the_header_never_panic() {
    let base = sample(true).build();
    for at in 0..80 {
        for bit in 0..8 {
            let mut m = base.clone();
            m[at] ^= 1 << bit;
            let r = catch_unwind(AssertUnwindSafe(|| exercise(m)));
            assert!(r.is_ok(), "panic flipping bit {bit} of header byte {at}");
        }
    }
}
