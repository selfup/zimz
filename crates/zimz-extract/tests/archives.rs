// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Extraction from real archives: the mwoffliner fixture (always) and the local library
//! (`ZIMZ_TEST_ZIM_DIR`), reporting how much boilerplate survives.

use std::path::{Path, PathBuf};

use zimz_core::Archive;
use zimz_extract::{Adapter, detect_adapter, extract};

fn fixture() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../fixtures/zim-testing-suite/data/nons/wikipedia_en_climate_change_mini_2024-06.zim",
    );
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipping: fixtures not fetched");
        None
    }
}

#[test]
fn climate_change_article() {
    let Some(path) = fixture() else { return };
    let a = Archive::open(&path).unwrap();
    assert_eq!(detect_adapter(&a), Adapter::MwOffliner);
    let e = a
        .entry_by_path(b'C', "Climate_change")
        .unwrap()
        .expect("article");
    let d = extract(&a, &e, Adapter::MwOffliner, Some("zim://climate/")).unwrap();
    assert_eq!(d.title, "Climate change");
    assert!(
        d.word_count > 300,
        "{} words (mini flavour: lead section only)",
        d.word_count
    );
    assert!(!d.markdown.contains("[edit]"));
    assert!(
        !d.markdown.to_lowercase().contains("retrieved "),
        "reference lines pruned: {}",
        d.markdown
            .lines()
            .find(|l| l.to_lowercase().contains("retrieved "))
            .unwrap_or("")
    );
    let internal = d.internal_links(true);
    assert!(internal.len() > 50, "{} internal links", internal.len());
    for (_, p) in internal.iter().take(30) {
        let found = a.entry_by_path(b'C', p).unwrap().is_some();
        assert!(found, "link target {p:?} must exist in the archive");
    }
    assert!(d.markdown.contains("](zim://climate/"));
    eprintln!(
        "Climate change: {} words, {} sections, {} links; first section {:?}",
        d.word_count,
        d.sections.len(),
        internal.len(),
        d.outline()[0]
    );
}

#[test]
fn every_html_entry_of_the_fixture_extracts() {
    let Some(path) = fixture() else { return };
    let a = Archive::open(&path).unwrap();
    let mut n = 0;
    let mut words = 0usize;
    for e in a.entries_in(a.namespace_range(b'C').unwrap()) {
        let e = e.unwrap();
        if !e.is_item() || !a.mime_type(&e).is_some_and(|m| m.starts_with("text/html")) {
            continue;
        }
        let d = extract(&a, &e, Adapter::MwOffliner, None).unwrap();
        assert!(!d.title.is_empty(), "{}", e.path);
        words += d.word_count;
        n += 1;
    }
    assert!(n > 100);
    eprintln!("{n} articles, {words} words");
}

#[test]
#[allow(clippy::cast_precision_loss)]
fn local_library_samples() {
    let Some(dir) = std::env::var_os("ZIMZ_TEST_ZIM_DIR").map(PathBuf::from) else {
        eprintln!("skipping: ZIMZ_TEST_ZIM_DIR not set");
        return;
    };
    let samples: &[(&str, &[&str])] = &[
        (
            "wikipedia_en_all_maxi_2026-02.zim",
            &["Zstd", "Leonardo_da_Vinci"],
        ),
        ("wikem_en_all_maxi_2021-02.zim", &["A/Acute_chest_pain"]),
        ("devdocs_en_git_2026-04.zim", &["git-rebase"]),
        (
            "www.mankier.com_en_all_2026-04.zim",
            &["www.mankier.com/1/cat"],
        ),
        (
            "gutenberg_en_lcc-pe_2026-03.zim",
            &["\"Stops\", Or How to Punctuate.20938"],
        ),
        ("libretexts.org_en_stats_2026-01.zim", &["index/page_10114"]),
        (
            "proofwiki_en_all_maxi_2026-04.zim",
            &["Pythagoras's_Theorem"],
        ),
    ];
    for (file, paths) in samples {
        let path = dir.join(file);
        if !path.is_file() {
            continue;
        }
        let a = Archive::open(&path).unwrap();
        let adapter = detect_adapter(&a);
        for p in *paths {
            let Some(e) = a.entry_by_path_compat(p).unwrap() else {
                panic!("{file}: {p} not found")
            };
            let e = a.resolve(&e).unwrap();
            let raw = a.item_data(&e).unwrap();
            let raw_text_len = zimz_extract::extract_html(
                &String::from_utf8_lossy(&raw),
                Adapter::Generic,
                None,
                &zimz_extract::RenderOptions {
                    base_namespace: e.namespace,
                    base_path: &e.path,
                    new_scheme: a.uses_new_namespace_scheme(),
                    link_prefix: None,
                    image_sources: false,
                },
            )
            .text
            .len();
            let d = extract(&a, &e, adapter, None).unwrap();
            assert!(d.word_count > 50, "{file} {p}: only {} words", d.word_count);
            assert_ne!(d.title, "");
            let kept = d.text.len() as f64 / raw_text_len.max(1) as f64;
            eprintln!(
                "{file} {p}: {adapter:?} title={:?} words={} sections={} links={} kept {:.0}% of the raw text",
                d.title,
                d.word_count,
                d.sections.len(),
                d.links.len(),
                kept * 100.0
            );
            if adapter == Adapter::MwOffliner {
                assert!(!d.markdown.contains("[edit]"));
            }
        }
    }
    // youtube: metadata + description
    let yt = Path::new(&dir).join("../Downloads/urban-prepper_en_all_2026-08.zim");
    if yt.is_file() {
        let a = Archive::open(&yt).unwrap();
        assert_eq!(detect_adapter(&a), Adapter::YouTube);
        let e = a
            .entries_in(a.namespace_range(b'C').unwrap())
            .map(Result::unwrap)
            .find(|e| e.path.starts_with("index/") && e.path != "index.html")
            .unwrap();
        let d = extract(&a, &e, Adapter::YouTube, None).unwrap();
        assert!(d.markdown.contains("## Description"), "{}", d.markdown);
        eprintln!(
            "youtube {}: {} words, source {}",
            e.path, d.word_count, d.source_path
        );
    }
}
