// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Tests against a real ZIM directory, enabled with `ZIMZ_TEST_ZIM_DIR=~/zims`.
//! They check the catalogue scan, federated latency and a 100-query soak for memory
//! growth. Numbers go to stderr so `--nocapture` shows them.
#![allow(clippy::cast_precision_loss)]

use std::path::PathBuf;
use std::time::Instant;

use zimz_search::{ContextRequest, Library, LibraryConfig, SearchRequest, SuggestRequest};

fn zim_dir() -> Option<PathBuf> {
    std::env::var_os("ZIMZ_TEST_ZIM_DIR").map(PathBuf::from)
}

fn rss_bytes() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
        * 1024
}

#[test]
fn scan_and_soak_local_library() {
    let Some(dir) = zim_dir() else {
        eprintln!("ZIMZ_TEST_ZIM_DIR not set; skipping");
        return;
    };
    let t0 = Instant::now();
    let lib = Library::scan(LibraryConfig {
        dirs: vec![dir],
        ..LibraryConfig::default()
    })
    .unwrap();
    eprintln!(
        "scanned {} archives ({} failures) in {:.1?}",
        lib.len(),
        lib.failures().len(),
        t0.elapsed()
    );
    for f in lib.failures() {
        eprintln!("  failed: {} — {}", f.file, f.error);
    }
    assert!(!lib.is_empty());
    let mut names: Vec<&str> = lib.archives().map(|a| a.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), lib.len(), "names are unique");

    let queries = [
        "photosynthesis",
        "git rebase interactive",
        "carbon dioxide",
        "quantum entanglement",
        "sourdough starter",
        "kubernetes pod scheduling",
        "french revolution causes",
        "mitochondria membrane",
        "rust borrow checker",
        "water purification methods",
    ];
    // Warm-up, then measure.
    let _ = lib.search(&SearchRequest::new("photosynthesis")).unwrap();
    let rss0 = rss_bytes();
    let mut worst = 0u64;
    for round in 0..10 {
        for q in &queries {
            let t = Instant::now();
            let res = lib.search(&SearchRequest::new(*q)).unwrap();
            let ms = t.elapsed().as_millis() as u64;
            worst = worst.max(ms);
            if round == 0 {
                eprintln!(
                    "search {q:?}: {} hits from {} archives, est {}, {ms} ms, top: {}",
                    res.hits.len(),
                    res.archives_searched.len(),
                    res.total_estimate,
                    res.hits
                        .first()
                        .map(|h| format!("{} [{}]", h.title, h.archive))
                        .unwrap_or_default()
                );
            }
        }
    }
    let t = Instant::now();
    let ctx = lib
        .context(&ContextRequest::new("carbon dioxide greenhouse effect"))
        .unwrap();
    eprintln!(
        "context: {} excerpts, {} chars, {:.1?}",
        ctx.excerpts.len(),
        ctx.chars_used,
        t.elapsed()
    );
    let t = Instant::now();
    let s = lib
        .suggest(&SuggestRequest {
            prefix: "carbon".into(),
            archives: vec![],
            limit: 10,
        })
        .unwrap();
    eprintln!(
        "suggest: {} suggestions in {:.1?}",
        s.suggestions.len(),
        t.elapsed()
    );
    let rss1 = rss_bytes();
    eprintln!(
        "soak: 100 searches, worst {worst} ms; RSS {:.0} MiB -> {:.0} MiB; caches {:?}",
        rss0 as f64 / 1_048_576.0,
        rss1 as f64 / 1_048_576.0,
        lib.extract_cache_stats()
    );
    // Cluster + extract caches are bounded (default 256 + 64 MiB); allow page-cache
    // noise from mmap on top of that.
    let budget =
        (lib.config().cluster_cache_bytes + lib.config().extract_cache_bytes) as u64 + (512 << 20);
    assert!(
        rss1.saturating_sub(rss0) < budget,
        "RSS grew by {} MiB during the soak",
        rss1.saturating_sub(rss0) >> 20
    );
}
