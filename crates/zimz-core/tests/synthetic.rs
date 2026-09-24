// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Behaviour tests on archives built in memory (`common::builder`), so every branch of
//! the reader can be targeted precisely.

mod common;

#[cfg(any(feature = "zstd-c", feature = "xz-c"))]
use common::builder::Codec;
use common::builder::{ZimBuilder, sample};
use common::open_bytes;
use common::open_bytes_with;
#[cfg(feature = "zstd-c")]
use zimz_core::Compression;
use zimz_core::integrity::{self, Check};
use zimz_core::{DirentKind, Error, OpenConfig, TitleIndex};

#[test]
fn builder_output_is_valid_for_both_schemes() {
    for new in [true, false] {
        let a = open_bytes(sample(new).build()).unwrap();
        assert_eq!(a.uses_new_namespace_scheme(), new);
        assert!(a.verify_checksum().unwrap());
        assert!(
            integrity::run(&a, &Check::ALL).is_empty(),
            "scheme new={new}"
        );
        assert_eq!(a.uuid(), "7a696d7a-2d74-6573-742d-757569642d21");
    }
}

#[test]
fn path_lookup_finds_every_entry_and_reports_insertion_points() {
    let a = open_bytes(sample(true).build()).unwrap();
    for i in 0..a.entry_count() {
        let e = a.entry(i).unwrap();
        assert_eq!(a.find_path(e.namespace, &e.path).unwrap(), (true, i));
        assert_eq!(
            a.entry_by_path(e.namespace, &e.path)
                .unwrap()
                .unwrap()
                .index,
            i
        );
    }
    // C is the first namespace: "Aardvark" sorts before everything
    assert_eq!(a.find_path(b'C', "Aardvark").unwrap(), (false, 0));
    assert_eq!(a.find_path(b'A', "anything").unwrap(), (false, 0));
    let c = a.namespace_range(b'C').unwrap();
    assert_eq!(a.find_path(b'C', "zzz").unwrap(), (false, c.end));
    assert_eq!(a.find_path(b'Z', "").unwrap(), (false, a.entry_count()));
    assert!(a.entry_by_path(b'C', "Nope").unwrap().is_none());
    assert!(a.entry_by_long_path("C/Apple").unwrap().is_some());
    assert!(a.entry_by_long_path("Apple").unwrap().is_none());
}

#[test]
fn namespace_ranges_cover_everything_in_order() {
    let a = open_bytes(sample(true).build()).unwrap();
    assert_eq!(a.namespaces().unwrap(), vec![b'C', b'M', b'W', b'X']);
    let mut next = 0;
    for ns in a.namespaces().unwrap() {
        let r = a.namespace_range(ns).unwrap();
        assert_eq!(
            r.start, next,
            "namespace {} must start where the previous ended",
            ns as char
        );
        assert!(!r.is_empty());
        for e in a.entries_in(r.clone()) {
            assert_eq!(e.unwrap().namespace, ns);
        }
        next = r.end;
    }
    assert_eq!(next, a.entry_count());
    assert!(a.namespace_range(b'Q').unwrap().is_empty());
    let old = open_bytes(sample(false).build()).unwrap();
    assert_eq!(old.namespaces().unwrap(), vec![b'A', b'I', b'M']);
}

#[test]
fn redirects_resolve_and_loops_are_bounded() {
    let a = open_bytes(sample(true).build()).unwrap();
    let r = a.entry_by_path(b'C', "Apples").unwrap().unwrap();
    assert!(r.is_redirect());
    assert_eq!(a.resolve(&r).unwrap().path, "Apple");
    assert!(matches!(a.item_data(&r), Err(Error::NotAnItem(_))));

    let b = ZimBuilder::new_scheme()
        .html("Target", "Target", "x")
        .redirect(b'C', "Hop1", "Hop1", b'C', "Hop2")
        .redirect(b'C', "Hop2", "Hop2", b'C', "Hop3")
        .redirect(b'C', "Hop3", "Hop3", b'C', "Target")
        .redirect(b'C', "Loop1", "Loop1", b'C', "Loop2")
        .redirect(b'C', "Loop2", "Loop2", b'C', "Loop1");
    let a = open_bytes(b.build()).unwrap();
    let hop = a.entry_by_path(b'C', "Hop1").unwrap().unwrap();
    assert_eq!(a.resolve(&hop).unwrap().path, "Target");
    let lp = a.entry_by_path(b'C', "Loop1").unwrap().unwrap();
    assert!(matches!(a.resolve(&lp), Err(Error::RedirectLoop(50))));
}

#[test]
fn main_page_via_w_entry_or_header() {
    let a = open_bytes(sample(true).build()).unwrap();
    let m = a.main_entry().unwrap().unwrap();
    assert_eq!(m.full_path(), "W/mainPage");
    assert!(m.is_redirect());
    assert_eq!(a.resolve(&m).unwrap().full_path(), "C/Home");
    assert!(
        a.header().has_main_page(),
        "header also points at W/mainPage"
    );

    let old = open_bytes(sample(false).build()).unwrap();
    let m = old.main_entry().unwrap().unwrap();
    assert_eq!(m.full_path(), "A/Home");
    assert!(m.is_item());

    let none = open_bytes(ZimBuilder::new_scheme().html("X", "X", "x").build()).unwrap();
    assert!(none.main_entry().unwrap().is_none());
    assert!(!none.header().has_main_page());
}

#[test]
fn metadata_counter_tags_and_article_count() {
    let a = open_bytes(sample(true).build()).unwrap();
    assert_eq!(
        a.metadata_string("Title").unwrap().as_deref(),
        Some("Sample")
    );
    assert_eq!(a.metadata_string("Nope").unwrap(), None);
    assert_eq!(
        a.metadata_keys().unwrap(),
        vec!["Counter", "Language", "Tags", "Title"]
    );
    assert_eq!(
        a.counter().unwrap(),
        vec![
            ("text/html".to_string(), 5),
            ("text/css".to_string(), 1),
            ("image/png".to_string(), 1)
        ]
    );
    // front articles: 5 html + 1 redirect
    assert_eq!(a.article_count().unwrap(), 6);

    let old = open_bytes(sample(false).build()).unwrap();
    assert_eq!(
        old.article_count().unwrap(),
        5,
        "old scheme without listing: text/html from Counter"
    );
    let bare = open_bytes(
        ZimBuilder::old_scheme()
            .html("A1", "A1", "x")
            .html("A2", "A2", "y")
            .item(b'I', "i.png", "", "image/png", "p")
            .build(),
    )
    .unwrap();
    assert_eq!(
        bare.article_count().unwrap(),
        2,
        "no Counter: size of the content namespace"
    );
}

#[test]
fn title_index_front_articles_listing() {
    let a = open_bytes(sample(true).build()).unwrap();
    assert!(matches!(
        a.title_index(),
        TitleIndex::FrontArticles { count: 6, .. }
    ));
    let titles: Vec<String> = a
        .title_ordered()
        .map(|e| e.unwrap().title().to_string())
        .collect();
    assert_eq!(
        titles,
        vec!["Apple", "Apple pie", "Apples", "Banana", "Home", "zebra"]
    );
    assert_eq!(a.find_title_prefix(b'C', "Apple").unwrap(), 0..3);
    assert_eq!(a.find_title_prefix(b'C', "App").unwrap(), 0..3);
    assert_eq!(a.find_title_prefix(b'C', "").unwrap(), 0..6);
    assert_eq!(
        a.find_title_prefix(b'C', "Zeb").unwrap().len(),
        0,
        "byte-wise, case-sensitive"
    );
    assert_eq!(a.find_title_prefix(b'C', "zeb").unwrap(), 5..6);
    assert_eq!(a.find_title_prefix(b'C', "zzzz").unwrap().len(), 0);
    assert_eq!(
        a.entry_by_title("Apple pie").unwrap().unwrap().path,
        "Apple_pie"
    );
    assert_eq!(
        a.entry_by_title("Apple pi").unwrap(),
        None,
        "exact match only"
    );
    assert_eq!(
        a.entry_by_title("style.css").unwrap(),
        None,
        "not a front article"
    );
}

#[test]
fn title_index_header_list_old_scheme() {
    let a = open_bytes(sample(false).build()).unwrap();
    assert!(
        matches!(a.title_index(), TitleIndex::Header { count, .. } if count == a.entry_count())
    );
    let keys: Vec<(u8, String)> = a
        .title_ordered()
        .map(|e| e.unwrap())
        .map(|e| (e.namespace, e.title().to_string()))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    assert_eq!(a.find_title_prefix(b'A', "Apple").unwrap().len(), 3);
    assert_eq!(a.find_title_prefix(b'I', "logo").unwrap().len(), 1);
    assert_eq!(a.find_title_prefix(b'M', "").unwrap().len(), 4);
    assert_eq!(
        a.entry_by_title("logo.png").unwrap().unwrap().full_path(),
        "I/logo.png",
        "old scheme falls back to I"
    );
    assert_eq!(
        a.entry_by_title("Title").unwrap(),
        None,
        "M is never searched"
    );
}

#[test]
fn no_title_index_at_all() {
    let mut b = ZimBuilder::new_scheme().html("A", "A", "x");
    b.v1_listing = false;
    let a = open_bytes(b.build()).unwrap();
    assert_eq!(a.title_index(), TitleIndex::None);
    assert!(a.title_index().is_empty());
    assert!(matches!(
        a.find_title_prefix(b'C', "A"),
        Err(Error::NoTitleIndex)
    ));
    assert!(matches!(
        a.entry_by_title_position(0),
        Err(Error::NoTitleIndex)
    ));
    assert_eq!(a.title_ordered().count(), 0);
    assert_eq!(a.article_count().unwrap(), 1);
}

#[test]
fn entry_by_path_compat_rules() {
    let a = open_bytes(sample(true).build()).unwrap();
    for p in [
        "Apple", "C/Apple", "/Apple", "/C/Apple", "A/Apple", "X/Apple",
    ] {
        assert_eq!(
            a.entry_by_path_compat(p).unwrap().map(|e| e.full_path()),
            Some("C/Apple".into()),
            "{p}"
        );
    }
    assert!(a.entry_by_path_compat("Nope").unwrap().is_none());
    assert!(
        a.entry_by_path_compat("M/Title").unwrap().is_none(),
        "new scheme only looks in C"
    );

    let old = open_bytes(sample(false).build()).unwrap();
    assert_eq!(
        old.entry_by_path_compat("A/Apple")
            .unwrap()
            .unwrap()
            .full_path(),
        "A/Apple"
    );
    assert_eq!(
        old.entry_by_path_compat("Apple")
            .unwrap()
            .unwrap()
            .full_path(),
        "A/Apple"
    );
    assert_eq!(
        old.entry_by_path_compat("logo.png")
            .unwrap()
            .unwrap()
            .full_path(),
        "I/logo.png"
    );
    assert_eq!(
        old.entry_by_path_compat("I/logo.png")
            .unwrap()
            .unwrap()
            .full_path(),
        "I/logo.png"
    );
    assert_eq!(
        old.entry_by_path_compat("M/Title")
            .unwrap()
            .unwrap()
            .full_path(),
        "M/Title"
    );
    assert_eq!(
        old.entry_by_path_compat("C/Apple")
            .unwrap()
            .unwrap()
            .full_path(),
        "A/Apple",
        "wrong namespace falls back"
    );
    assert!(old.entry_by_path_compat("Nope").unwrap().is_none());
}

#[test]
fn lookup_grids_agree_with_plain_binary_search() {
    let mut b = ZimBuilder::new_scheme();
    for i in 0..500 {
        b = b.html(
            &format!("Page_{:04}", i * 7 % 500),
            &format!("Title {:04}", i * 13 % 500),
            "x",
        );
    }
    for i in 0..60 {
        b = b.item(b'M', &format!("Meta{i:02}"), "", "text/plain", "m");
    }
    let bytes = b.build();
    let plain = open_bytes_with(
        bytes.clone(),
        OpenConfig {
            lookup_bucket: 0,
            ..OpenConfig::default()
        },
    )
    .unwrap();
    for bucket in [1u32, 2, 7, 64, 499, 512, 10_000] {
        let gridded = open_bytes_with(
            bytes.clone(),
            OpenConfig {
                lookup_bucket: bucket,
                ..OpenConfig::default()
            },
        )
        .unwrap();
        for i in 0..plain.entry_count() {
            let e = plain.entry(i).unwrap();
            assert_eq!(
                gridded.find_path(e.namespace, &e.path).unwrap(),
                (true, i),
                "bucket {bucket}"
            );
            // misses: neighbours of every key, in every namespace
            for probe in [
                format!("{}!", e.path),
                format!("{}~", e.path),
                e.path[..e.path.len() - 1].to_string(),
            ] {
                for ns in *b"ACMZ" {
                    assert_eq!(
                        gridded.find_path(ns, &probe).unwrap(),
                        plain.find_path(ns, &probe).unwrap(),
                        "bucket {bucket} {ns} {probe}"
                    );
                }
            }
        }
        for pos in 0..plain.title_index().len() {
            let e = plain.entry_by_title_position(pos).unwrap();
            for t in [
                e.title().to_string(),
                format!("{}!", e.title()),
                "Title".to_string(),
                String::new(),
                "zzz".to_string(),
            ] {
                for ns in *b"CM" {
                    assert_eq!(
                        gridded.title_lower_bound(ns, t.as_bytes()).unwrap(),
                        plain.title_lower_bound(ns, t.as_bytes()).unwrap(),
                        "bucket {bucket} {ns} {t:?}"
                    );
                }
            }
            assert_eq!(
                gridded.entry_by_title(e.title()).unwrap().unwrap().index,
                e.index
            );
        }
        assert_eq!(
            gridded.find_title_prefix(b'C', "Title 00").unwrap(),
            plain.find_title_prefix(b'C', "Title 00").unwrap()
        );
    }
}

#[test]
fn user_path_matches_libzim_semantics() {
    let a = open_bytes(sample(true).build()).unwrap();
    let e = a.entry_by_path(b'C', "Apple").unwrap().unwrap();
    assert_eq!(a.user_path(&e), "Apple");
    let old = open_bytes(sample(false).build()).unwrap();
    let e = old.entry_by_path(b'A', "Apple").unwrap().unwrap();
    assert_eq!(old.user_path(&e), "A/Apple");
}

#[test]
fn direct_access_only_for_uncompressed_clusters() {
    let a = open_bytes(sample(true).build()).unwrap();
    let e = a.entry_by_path(b'C', "style.css").unwrap().unwrap();
    let da = a.direct_access(&e).unwrap().expect("uncompressed archive");
    assert_eq!(da.len, 6);
    assert_eq!(&*a.source().slice(da.offset, 6).unwrap(), b"body{}");
    assert_eq!(&*a.item_data(&e).unwrap(), b"body{}");
    assert_eq!(a.item_size(&e).unwrap(), 6);
    let r = a.entry_by_path(b'C', "Apples").unwrap().unwrap();
    assert!(matches!(a.direct_access(&r), Err(Error::NotAnItem(_))));
}

#[cfg(feature = "zstd-c")]
#[test]
fn compressed_archive_mixes_direct_and_cached_blobs() {
    let mut b = sample(true)
        .item(
            b'C',
            "raw.bin",
            "",
            "application/octet-stream",
            vec![1, 2, 3],
        )
        .uncompressed();
    b.codec = Codec::Zstd;
    let a = open_bytes(b.build()).unwrap();
    let html = a.entry_by_path(b'C', "Apple").unwrap().unwrap();
    assert!(
        a.direct_access(&html).unwrap().is_none(),
        "compressed cluster"
    );
    assert_eq!(
        a.cluster_compression(html.location().unwrap().0).unwrap(),
        Compression::Zstd
    );
    assert!(a.item_data(&html).unwrap().starts_with(b"<html>"));
    assert_eq!(
        a.item_data(&html).unwrap().into_vec().len(),
        a.item_size(&html).unwrap() as usize
    );
    let raw = a.entry_by_path(b'C', "raw.bin").unwrap().unwrap();
    assert!(
        a.direct_access(&raw).unwrap().is_some(),
        ".uncompressed() item"
    );
    assert_eq!(a.item_data(&raw).unwrap().into_vec(), vec![1, 2, 3]);
    let listing = a
        .entry_by_path(b'X', "listing/titleOrdered/v1")
        .unwrap()
        .unwrap();
    assert!(
        a.direct_access(&listing).unwrap().is_some(),
        "listings are always uncompressed"
    );
    assert!(integrity::run(&a, &Check::ALL).is_empty());
    assert!(a.cluster_cache_stats().0 >= 1);
}

#[cfg(all(feature = "zstd-c", feature = "xz-c"))]
#[test]
fn zstd_and_xz_clusters_roundtrip_with_many_small_clusters() {
    for codec in [Codec::None, Codec::Zstd, Codec::Xz] {
        for extended in [false, true] {
            let mut b = ZimBuilder::new_scheme();
            b.codec = codec;
            b.extended = extended;
            b.cluster_limit = 300;
            for i in 0..40 {
                b = b.html(
                    &format!("Page{i:02}"),
                    &format!("Page {i:02}"),
                    &"lorem ipsum ".repeat(i + 1),
                );
            }
            let a = open_bytes(b.build()).unwrap();
            assert!(
                a.cluster_count() > 10,
                "{codec:?}: {} clusters",
                a.cluster_count()
            );
            for i in 0..40 {
                let e = a
                    .entry_by_path(b'C', &format!("Page{i:02}"))
                    .unwrap()
                    .unwrap();
                let data = a.item_data(&e).unwrap();
                assert!(
                    data.ends_with(
                        format!("{}</body></html>", "lorem ipsum ".repeat(i + 1)).as_bytes()
                    ),
                    "{codec:?} page {i}"
                );
                let (c, _) = e.location().unwrap();
                assert_eq!(a.cluster(c).unwrap().is_extended(), extended);
                let expected = match codec {
                    Codec::None => Compression::None,
                    Codec::Zstd => Compression::Zstd,
                    Codec::Xz => Compression::Xz,
                };
                assert_eq!(a.cluster_compression(c).unwrap(), expected);
            }
            assert!(
                integrity::run(&a, &Check::ALL).is_empty(),
                "{codec:?} extended={extended}"
            );
        }
    }
}

#[cfg(feature = "zstd-c")]
#[test]
fn cluster_cache_respects_its_byte_budget() {
    let mut b = ZimBuilder::new_scheme();
    b.codec = Codec::Zstd;
    b.cluster_limit = 1500;
    for i in 0..60 {
        b = b.html(
            &format!("P{i}"),
            &format!("P{i}"),
            &format!("{i:04}").repeat(200),
        );
    }
    let bytes = b.build();
    let config = OpenConfig {
        cluster_cache_bytes: 4000,
        ..OpenConfig::default()
    };
    let a = open_bytes_with(bytes, config).unwrap();
    assert!(a.cluster_count() >= 20);
    for round in 0..2 {
        for i in 0..60 {
            let e = a.entry_by_path(b'C', &format!("P{i}")).unwrap().unwrap();
            assert!(
                a.item_data(&e).unwrap().ends_with(
                    format!("{}</body></html>", format!("{i:04}").repeat(200)).as_bytes()
                ),
                "round {round} page {i}"
            );
            let (cached, bytes) = a.cluster_cache_stats();
            assert!(
                cached <= 5 && bytes <= 4000,
                "cache grew to {cached} clusters / {bytes} bytes"
            );
        }
    }
}

#[cfg(feature = "zstd-c")]
#[test]
fn oversized_compressed_cluster_is_rejected() {
    let mut b = ZimBuilder::new_scheme().html("Big", "Big", &"x".repeat(10_000));
    b.codec = Codec::Zstd;
    let bytes = b.build();
    let config = OpenConfig {
        max_cluster_bytes: 100,
        ..OpenConfig::default()
    };
    let a = open_bytes_with(bytes.clone(), config.clone()).unwrap();
    let e = a.entry_by_path(b'C', "Big").unwrap().unwrap();
    assert!(matches!(
        a.item_data(&e),
        Err(Error::ClusterTooLarge { limit: 100, .. })
    ));
    let problems = integrity::run(&a, &[Check::Clusters]);
    assert_eq!(problems.len(), 1);
    // uncompressed clusters are served from the file and are never subject to the cap
    let plain = open_bytes_with(
        ZimBuilder::new_scheme()
            .html("Big", "Big", &"x".repeat(10_000))
            .build(),
        config,
    )
    .unwrap();
    let e = plain.entry_by_path(b'C', "Big").unwrap().unwrap();
    assert!(plain.item_data(&e).unwrap().len() > 10_000);
}

#[test]
fn huge_dirents_are_read_through_the_widest_window() {
    let path = "p".repeat(70_000);
    let title = "t".repeat(3_000);
    let a = open_bytes(
        ZimBuilder::new_scheme()
            .html(&path, &title, "big")
            .html("Small", "Small", "s")
            .build(),
    )
    .unwrap();
    let e = a.entry_by_path(b'C', &path).unwrap().unwrap();
    assert_eq!(e.title(), title);
    assert_eq!(a.entry_by_title(&title).unwrap().unwrap().index, e.index);
    assert!(integrity::run(&a, &Check::ALL).is_empty());
}

#[test]
fn empty_title_means_path() {
    let a = open_bytes(
        ZimBuilder::new_scheme()
            .html("Same", "Same", "x")
            .html("Other", "Different", "y")
            .build(),
    )
    .unwrap();
    let same = a.entry_by_path(b'C', "Same").unwrap().unwrap();
    assert_eq!(same.title, "", "stored empty when equal to the path");
    assert_eq!(same.title(), "Same");
    let other = a.entry_by_path(b'C', "Other").unwrap().unwrap();
    assert_eq!(other.title, "Different");
}

#[test]
fn checksum_detects_corruption_and_truncation() {
    let layout = sample(true).build_layout();
    let mut bytes = layout.bytes.clone();
    let item = a_blob_offset(&layout);
    bytes[item] ^= 0xff;
    let a = open_bytes(bytes).unwrap();
    assert!(!a.verify_checksum().unwrap());
    assert_eq!(integrity::run(&a, &[Check::Checksum]).len(), 1);
    assert_eq!(
        integrity::run(&a, &Check::QUICK).len(),
        0,
        "structure is still intact"
    );

    let mut truncated = layout.bytes.clone();
    truncated.pop();
    assert!(matches!(open_bytes(truncated), Err(Error::Corrupt(_))));
    let mut grown = layout.bytes.clone();
    grown.push(0);
    assert!(matches!(open_bytes(grown), Err(Error::Corrupt(_))));
}

/// Absolute offset of the first byte of the first uncompressed cluster's first blob.
fn a_blob_offset(layout: &common::builder::Layout) -> usize {
    let cluster = layout.cluster_offsets[0] as usize;
    let first =
        u32::from_le_bytes(layout.bytes[cluster + 1..cluster + 5].try_into().unwrap()) as usize;
    cluster + 1 + first
}

fn set_u64(bytes: &mut [u8], at: u64, v: u64) {
    bytes[at as usize..at as usize + 8].copy_from_slice(&v.to_le_bytes());
}

fn set_u32(bytes: &mut [u8], at: u64, v: u32) {
    bytes[at as usize..at as usize + 4].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn each_integrity_check_catches_its_own_corruption() {
    let layout = sample(false).build_layout();
    let n = layout.order.len() as u64;
    let size = layout.bytes.len() as u64;

    // dirent order: swap two pointers
    let mut b = layout.bytes.clone();
    let p1 = u64::from_le_bytes(
        b[layout.path_ptr_pos as usize + 8..][..8]
            .try_into()
            .unwrap(),
    );
    let p2 = u64::from_le_bytes(
        b[layout.path_ptr_pos as usize + 16..][..8]
            .try_into()
            .unwrap(),
    );
    set_u64(&mut b, layout.path_ptr_pos + 8, p2);
    set_u64(&mut b, layout.path_ptr_pos + 16, p1);
    if let Ok(a) = open_bytes(b) {
        let p = integrity::run(&a, &[Check::DirentOrder]);
        assert_eq!(p.len(), 1, "{p:?}");
        assert_eq!(p[0].check, Check::DirentOrder);
    }

    // dirent pointer beyond the file
    let mut b = layout.bytes.clone();
    set_u64(&mut b, layout.path_ptr_pos + 8 * (n - 1), size + 100);
    match open_bytes(b) {
        Err(Error::Corrupt(_)) => {}
        Err(e) => panic!("unexpected {e}"),
        Ok(a) => assert_eq!(
            integrity::run(&a, &[Check::DirentPointers])[0].check,
            Check::DirentPointers
        ),
    }

    // an item with a MIME index past the list
    let mut b = layout.bytes.clone();
    let css = layout.index_of(b'A', "style.css");
    let off = layout.dirent_offsets[css as usize];
    b[off as usize..off as usize + 2].copy_from_slice(&0xfff0u16.to_le_bytes());
    let a = open_bytes(b).unwrap();
    let p = integrity::run(&a, &[Check::DirentMimeTypes]);
    assert_eq!(p.len(), 1);
    assert!(p[0].message.contains("A/style.css"), "{}", p[0].message);
    assert_eq!(a.mime_type(&a.entry(css).unwrap()), None);

    // cluster pointer beyond the file
    let mut b = layout.bytes.clone();
    set_u64(&mut b, layout.cluster_ptr_pos, size + 1);
    let a = open_bytes(b).unwrap();
    assert_eq!(
        integrity::run(&a, &[Check::ClusterPointers])[0].check,
        Check::ClusterPointers
    );
    assert!(matches!(a.cluster(0), Err(Error::Corrupt(_))));

    // title list: index out of range, then wrong order
    let mut b = layout.bytes.clone();
    set_u32(&mut b, layout.title_ptr_pos, n as u32 + 5);
    let a = open_bytes(b).unwrap();
    assert_eq!(
        integrity::run(&a, &[Check::TitleIndex])[0].check,
        Check::TitleIndex
    );
    assert!(matches!(
        a.entry_by_title_position(0),
        Err(Error::Corrupt(_))
    ));
    let mut b = layout.bytes.clone();
    let t0 = u32::from_le_bytes(b[layout.title_ptr_pos as usize..][..4].try_into().unwrap());
    let t1 = u32::from_le_bytes(
        b[layout.title_ptr_pos as usize + 4..][..4]
            .try_into()
            .unwrap(),
    );
    set_u32(&mut b, layout.title_ptr_pos, t1);
    set_u32(&mut b, layout.title_ptr_pos + 4, t0);
    let a = open_bytes(b).unwrap();
    assert_eq!(
        integrity::run(&a, &[Check::TitleIndex])[0].check,
        Check::TitleIndex
    );

    // misaligned first blob offset in cluster 0
    let mut b = layout.bytes.clone();
    set_u32(&mut b, layout.cluster_offsets[0] + 1, 7);
    let a = open_bytes(b).unwrap();
    let p = integrity::run(&a, &[Check::Clusters]);
    assert_eq!(p[0].check, Check::Clusters);
    assert!(matches!(a.cluster(0), Err(Error::Corrupt(_))));
    assert!(integrity::run(&a, &[Check::DirentOrder, Check::ClusterPointers]).is_empty());
}

#[test]
fn out_of_range_requests_are_errors_not_panics() {
    let a = open_bytes(sample(true).build()).unwrap();
    assert!(matches!(a.entry(a.entry_count()), Err(Error::Corrupt(_))));
    assert!(matches!(a.entry(u32::MAX), Err(Error::Corrupt(_))));
    assert!(matches!(
        a.dirent_offset(a.entry_count()),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(
        a.cluster(a.cluster_count()),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(a.cluster_offset(u32::MAX), Err(Error::Corrupt(_))));
    assert!(matches!(a.blob(0, 999), Err(Error::Corrupt(_))));
    assert!(matches!(
        a.entry_by_title_position(a.title_index().len()),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(
        a.title_entry_index(u32::MAX),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(
        a.source().slice(a.size(), 1),
        Err(Error::OutOfBounds { .. })
    ));
    assert!(matches!(
        a.source().slice(u64::MAX, 1),
        Err(Error::OutOfBounds { .. })
    ));
}

#[test]
fn illustration_and_favicon_fallback() {
    let png = vec![0x89, b'P', b'N', b'G', 0, 0];
    let a = open_bytes(
        ZimBuilder::new_scheme()
            .html("H", "H", "h")
            .item(b'M', "Illustration_48x48@1", "", "image/png", png.clone())
            .build(),
    )
    .unwrap();
    assert_eq!(a.illustration(48, 48, 1.0).unwrap(), Some(png.clone()));
    assert_eq!(a.illustration(96, 96, 1.0).unwrap(), None);
    let sizes = a.illustration_sizes().unwrap();
    assert_eq!(
        (sizes[0].width, sizes[0].height, sizes[0].scale),
        (48, 48, 1.0)
    );

    let old = open_bytes(
        ZimBuilder::old_scheme()
            .html("H", "H", "h")
            .item(b'-', "favicon", "", "image/png", png.clone())
            .build(),
    )
    .unwrap();
    assert_eq!(
        old.illustration(48, 48, 1.0).unwrap(),
        Some(png),
        "old scheme falls back to -/favicon"
    );
    assert!(old.illustration_sizes().unwrap().is_empty());
    let none = open_bytes(ZimBuilder::new_scheme().html("H", "H", "h").build()).unwrap();
    assert_eq!(none.illustration(48, 48, 1.0).unwrap(), None);
}

#[test]
fn kinds_mime_types_and_iterators() {
    let a = open_bytes(sample(true).build()).unwrap();
    let css = a.entry_by_path(b'C', "style.css").unwrap().unwrap();
    assert_eq!(a.mime_type(&css), Some("text/css"));
    assert!(matches!(css.kind, DirentKind::Item { .. }));
    let r = a.entry_by_path(b'C', "Apples").unwrap().unwrap();
    assert_eq!(a.mime_type(&r), None);
    assert!(a.mime_list().iter().any(|m| m == "text/html"));
    assert_eq!(a.entries().count() as u32, a.entry_count());
    assert_eq!(a.entries_in(2..5).count(), 3);
    assert!(a.stored_checksum().is_ok());
    assert!(!a.has_fulltext_index() && !a.has_title_xapian_index());
    assert!(format!("{a:?}").contains("entries"));
}

#[test]
fn embedded_index_entries_are_located() {
    let glass = b"\x0f\x0dXapian Glass\x04n and then some bytes".to_vec();
    let a = open_bytes(
        ZimBuilder::new_scheme()
            .html("H", "H", "h")
            .item(
                b'X',
                "fulltext/xapian",
                "",
                "application/octet-stream+xapian",
                glass.clone(),
            )
            .item(
                b'X',
                "title/xapian",
                "",
                "application/octet-stream+xapian",
                b"title-db".to_vec(),
            )
            .build(),
    )
    .unwrap();
    let ft = a.fulltext_index().unwrap().expect("fulltext");
    assert_eq!(
        &*a.source().slice(ft.offset, ft.len as usize).unwrap(),
        &glass[..]
    );
    assert!(a.has_title_xapian_index());
    let legacy = open_bytes(
        ZimBuilder::old_scheme()
            .html("H", "H", "h")
            .item(
                b'Z',
                "/fulltextIndex/xapian",
                "",
                "application/octet-stream+xapian",
                b"legacy".to_vec(),
            )
            .build(),
    )
    .unwrap();
    assert_eq!(legacy.fulltext_index().unwrap().unwrap().len, 6);
    assert!(!legacy.has_title_xapian_index());
}

#[test]
fn split_archive_from_temp_parts_equals_whole() {
    let bytes = sample(true).build();
    let dir = std::env::temp_dir().join(format!("zimz-split-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let chunk = bytes.len() / 3 + 1;
    for (i, part) in bytes.chunks(chunk).enumerate() {
        let suffix = format!(
            "{}{}",
            (b'a' + (i / 26) as u8) as char,
            (b'a' + (i % 26) as u8) as char
        );
        std::fs::write(dir.join(format!("sample.zim{suffix}")), part).unwrap();
    }
    let whole = open_bytes(bytes.clone()).unwrap();
    for name in ["sample.zimaa", "sample.zim"] {
        let split = zimz_core::Archive::open(dir.join(name)).unwrap();
        assert_eq!(split.size(), whole.size());
        assert_eq!(split.uuid(), whole.uuid());
        assert!(split.verify_checksum().unwrap());
        for (x, y) in whole.entries().zip(split.entries()) {
            let (x, y) = (x.unwrap(), y.unwrap());
            assert_eq!(*x, *y);
            if x.is_item() {
                assert_eq!(
                    &*whole.item_data(&x).unwrap(),
                    &*split.item_data(&y).unwrap()
                );
            }
        }
        assert!(integrity::run(&split, &Check::ALL).is_empty());
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `cargo test -p zimz-core --test synthetic -- --ignored write_synthetic` writes the
/// builder's output to `target/synthetic/`; `uv run scripts/validate_synthetic.py` then
/// checks every file with python-libzim (CI does both).
#[test]
#[ignore = "writes files under target/ for validation with python-libzim"]
fn write_synthetic_archives_for_external_validation() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/synthetic");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("new-none.zim"), sample(true).build()).unwrap();
    std::fs::write(dir.join("old-none.zim"), sample(false).build()).unwrap();
    #[cfg(feature = "zstd-c")]
    {
        let mut b = sample(true);
        b.codec = Codec::Zstd;
        std::fs::write(dir.join("new-zstd.zim"), b.build()).unwrap();
        let mut b = sample(true);
        b.codec = Codec::Zstd;
        b.extended = true;
        std::fs::write(dir.join("new-zstd-extended.zim"), b.build()).unwrap();
    }
    #[cfg(feature = "xz-c")]
    {
        let mut b = sample(false);
        b.codec = Codec::Xz;
        std::fs::write(dir.join("old-xz.zim"), b.build()).unwrap();
    }
    eprintln!("wrote {}", dir.display());
}
