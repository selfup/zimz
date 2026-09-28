// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Smoke test over a real ZIM collection. Set `ZIMZ_TEST_ZIM_DIR=~/zims` to enable;
//! `ZIMZ_TEST_HEAVY=1` also verifies checksums of files larger than 256 MiB.

mod common;

use std::time::Instant;

use zimz_core::{Archive, TitleIndex};

#[test]
fn every_archive_opens_and_reads() {
    let Some(dir) = common::local_library() else {
        return;
    };
    let heavy = std::env::var_os("ZIMZ_TEST_HEAVY").is_some();
    let files = common::zim_files(&dir);
    assert!(!files.is_empty(), "no .zim files in {}", dir.display());
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let t = Instant::now();
        let archive = Archive::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let opened = t.elapsed();

        let main = archive
            .main_entry()
            .unwrap()
            .unwrap_or_else(|| panic!("{name}: no main page"));
        let main = archive.resolve(&main).unwrap();
        assert!(main.is_item(), "{name}: main page is not an item");
        let html = archive.item_data(&main).unwrap();
        assert!(!html.is_empty(), "{name}: empty main page");

        let keys = archive.metadata_keys().unwrap();
        assert!(
            keys.iter().any(|k| k == "Title" || k == "Name"),
            "{name}: no Title/Name metadata: {keys:?}"
        );
        let title = archive
            .metadata_string("Title")
            .unwrap()
            .unwrap_or_default();

        // read the first content entries, following redirects
        let ns = archive.content_namespace();
        let range = archive.namespace_range(ns).unwrap();
        assert!(!range.is_empty(), "{name}: empty content namespace");
        let mut read = 0usize;
        for e in archive.entries_in(range.start..range.end.min(range.start + 25)) {
            let e = e.unwrap();
            let e = archive.resolve(&e).unwrap();
            if e.is_item() {
                read += archive.item_data(&e).unwrap().len();
            }
        }

        // title index sanity: ordered for the first positions, prefix search works
        let ti = archive.title_index();
        if ti != TitleIndex::None {
            let mut prev: Option<(u8, String)> = None;
            for pos in 0..ti.len().min(200) {
                let e = archive.entry_by_title_position(pos).unwrap();
                let key = (e.namespace, e.title().to_string());
                if let Some(p) = &prev {
                    assert!(p <= &key, "{name}: title order broken at position {pos}");
                }
                prev = Some(key);
            }
            let probe = archive.entry_by_title_position(ti.len() / 2).unwrap();
            let prefix: String = probe.title().chars().take(4).collect();
            let range = archive.find_title_prefix(probe.namespace, &prefix).unwrap();
            assert!(
                range.contains(&(ti.len() / 2)),
                "{name}: prefix range {range:?} misses its own entry"
            );
        }

        let ft = archive.fulltext_index().unwrap();
        let tx = archive.title_xapian_index().unwrap();
        for da in [ft, tx].into_iter().flatten() {
            let head = archive.source().slice(da.offset, 14).unwrap();
            assert_eq!(
                &head[..],
                b"\x0f\x0dXapian Glass",
                "{name}: index blob is not a glass db"
            );
        }

        let mut checksum = String::from("skipped");
        if heavy || archive.size() < 256 << 20 {
            let t = Instant::now();
            let ok = archive.verify_checksum().unwrap();
            assert!(ok, "{name}: checksum mismatch");
            checksum = format!("ok in {:.1?}", t.elapsed());
        }
        eprintln!(
            "{name}: v{}.{} {:?} entries={} title_index={:?} ft={} tx={} read={}B open={:.1?} checksum={checksum}",
            archive.header().major,
            archive.header().minor,
            title,
            archive.entry_count(),
            ti.len(),
            ft.map_or(0, |d| d.len),
            tx.map_or(0, |d| d.len),
            read,
            opened
        );
    }
}
