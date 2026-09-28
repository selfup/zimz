// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Rough timings against the targets in PLAN.md section 6.9:
//! `cargo run --release -p zimz-core --example bench -- <file.zim> [samples]`

use std::time::Instant;

use zimz_core::Archive;

fn main() -> zimz_core::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: bench <file.zim> [samples]");
    let samples: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(2000);

    let t = Instant::now();
    let archive = Archive::open(&path)?;
    println!(
        "open: {:.2?} ({} entries, {} clusters)",
        t.elapsed(),
        archive.entry_count(),
        archive.cluster_count()
    );

    // deterministic pseudo-random entry indexes
    let n = archive.entry_count();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % u64::from(n)) as u32
    };
    let indexes: Vec<u32> = (0..samples).map(|_| next()).collect();

    let t = Instant::now();
    let entries: Vec<_> = indexes
        .iter()
        .map(|&i| archive.entry(i))
        .collect::<Result<_, _>>()?;
    println!(
        "entry by index: {:.2?} avg over {samples}",
        t.elapsed() / samples
    );

    let t = Instant::now();
    for e in &entries {
        let found = archive.entry_by_path(e.namespace, &e.path)?.expect("found");
        assert_eq!(found.index, e.index);
    }
    println!(
        "path lookup (binary search): {:.2?} avg",
        t.elapsed() / samples
    );

    let items: Vec<_> = entries
        .iter()
        .filter(|e| e.is_item())
        .take(200)
        .cloned()
        .collect();
    let t = Instant::now();
    let mut bytes = 0usize;
    for e in &items {
        bytes += archive.item_data(e)?.len();
    }
    let cold = t.elapsed();
    println!(
        "item read, cold clusters: {:.2?} avg over {} items ({} KiB total)",
        cold / items.len() as u32,
        items.len(),
        bytes / 1024
    );
    let t = Instant::now();
    for e in &items {
        bytes += std::hint::black_box(archive.item_data(e)?.len());
    }
    println!(
        "item read, warm cache: {:.2?} avg ({} KiB re-read)",
        t.elapsed() / items.len() as u32,
        bytes / 1024
    );

    if !archive.title_index().is_empty() {
        let t = Instant::now();
        let mut hits = 0;
        for e in entries.iter().take(500) {
            let prefix: String = e.title().chars().take(3).collect();
            hits += archive.find_title_prefix(e.namespace, &prefix)?.len();
        }
        println!(
            "title prefix range: {:.2?} avg ({hits} total hits)",
            t.elapsed() / 500
        );
    }
    let (cached, cache_bytes) = archive.cluster_cache_stats();
    println!(
        "cluster cache: {cached} clusters, {} MiB",
        cache_bytes >> 20
    );
    Ok(())
}
