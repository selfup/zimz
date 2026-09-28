// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Where does a federated query spend its time? `cargo run --release -p zimz-search
//! --example profile -- ~/zims "kubernetes pod scheduling"`.

use std::time::Instant;

use zimz_search::{ContextRequest, Library, LibraryConfig, SearchRequest};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("usage: profile <zim-dir> <query>");
    let query = args.next().expect("usage: profile <zim-dir> <query>");
    let t = Instant::now();
    let lib = Library::scan(LibraryConfig {
        dirs: vec![dir.into()],
        ..LibraryConfig::default()
    })
    .unwrap();
    println!("scan: {} archives in {:.1?}", lib.len(), t.elapsed());

    let mut per_archive = Vec::new();
    for a in lib.archives() {
        let mut req = SearchRequest::new(&query);
        req.archives = vec![a.name.clone()];
        req.snippet_chars = 0;
        req.or_fallback = false;
        let t = Instant::now();
        let r = lib.search(&req).unwrap();
        per_archive.push((t.elapsed(), a.name.clone(), r.total_estimate, r.hits.len()));
    }
    per_archive.sort_by_key(|a| std::cmp::Reverse(a.0));
    println!("slowest archives (AND, no snippets):");
    for (d, name, total, n) in per_archive.iter().take(8) {
        println!("  {d:>9.1?}  {name:<40} est {total:>7}  hits {n}");
    }

    for (label, snippet, or_fallback) in [
        ("all archives, no snippets, no OR", 0, false),
        ("all archives, no snippets, OR fallback", 0, true),
        ("all archives, snippets 300", 300, true),
    ] {
        let mut req = SearchRequest::new(&query);
        req.snippet_chars = snippet;
        req.or_fallback = or_fallback;
        let t = Instant::now();
        let r = lib.search(&req).unwrap();
        println!(
            "{label}: {:.1?} (fallback {}), top: {}",
            t.elapsed(),
            r.fallback_used,
            r.hits
                .iter()
                .take(3)
                .map(|h| format!("{} [{}] {:.2}", h.title, h.archive, h.score))
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
    // second run: caches warm
    let mut req = SearchRequest::new(&query);
    let t = Instant::now();
    let _ = lib.search(&req).unwrap();
    println!("repeat with snippets (warm): {:.1?}", t.elapsed());
    req.snippet_chars = 0;

    let t = Instant::now();
    let c = lib.context(&ContextRequest::new(&query)).unwrap();
    println!(
        "context: {:.1?}, {} excerpts, {} chars; sources: {}",
        t.elapsed(),
        c.excerpts.len(),
        c.chars_used,
        c.excerpts
            .iter()
            .map(|e| format!("{} ({} ch)", e.uri, e.chars))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let t = Instant::now();
    let _ = lib.context(&ContextRequest::new(&query)).unwrap();
    println!("context repeat (warm): {:.1?}", t.elapsed());
}
