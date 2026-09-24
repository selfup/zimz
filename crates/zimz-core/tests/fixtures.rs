// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Tests against the openZIM testing suite (`scripts/fetch-fixtures.sh`).

mod common;

use std::path::Path;

use zimz_core::header::MAGIC;
use zimz_core::integrity::{self, Check};
use zimz_core::source::FileRange;
use zimz_core::{Archive, OpenConfig, TitleIndex};

const FLAVOURS: [&str; 3] = ["withns", "nons", "noTitleListingV0"];

/// Read every item, verify checksum and title ordering, resolve the main page.
fn exercise(archive: &Archive) {
    assert!(archive.entry_count() > 0);
    assert!(
        archive.verify_checksum().unwrap(),
        "checksum of {}",
        archive.describe()
    );
    let main = archive.main_entry().unwrap().expect("main page");
    let main = archive.resolve(&main).unwrap();
    assert!(main.is_item());
    let html = archive.item_data(&main).unwrap();
    assert!(!html.is_empty());

    let mut items = 0;
    let mut bytes = 0usize;
    for entry in archive.entries() {
        let entry = entry.unwrap();
        if entry.is_item() {
            let data = archive.item_data(&entry).unwrap();
            assert_eq!(data.len() as u64, archive.item_size(&entry).unwrap());
            bytes += data.len();
            items += 1;
            assert!(
                archive.mime_type(&entry).is_some(),
                "mime for {}",
                entry.full_path()
            );
        } else if let Some(target) = entry.redirect_target() {
            assert!(target < archive.entry_count());
        }
    }
    assert!(items > 0 && bytes > 0);
    assert!(integrity::run(archive, &Check::ALL).is_empty());

    if archive.title_index() != TitleIndex::None {
        let mut prev: Option<(u8, String)> = None;
        for entry in archive.title_ordered() {
            let entry = entry.unwrap();
            let key = (entry.namespace, entry.title().to_string());
            if let Some(p) = &prev {
                assert!(p <= &key, "title order broken at {p:?} > {key:?}");
            }
            prev = Some(key);
        }
        // every prefix range found by binary search really starts with the prefix
        let ns = archive.content_namespace();
        let first = archive.find_title_prefix(ns, "").unwrap();
        assert!(!first.is_empty());
        let sample = archive
            .entry_by_title_position(first.start + first.len() as u32 / 2)
            .unwrap();
        let prefix: String = sample.title().chars().take(3).collect();
        let range = archive.find_title_prefix(ns, &prefix).unwrap();
        assert!(
            !range.is_empty(),
            "prefix {prefix:?} must match at least its own entry"
        );
        for pos in range.clone() {
            let e = archive.entry_by_title_position(pos).unwrap();
            assert!(
                e.title().starts_with(&prefix),
                "{:?} in range for {prefix:?}",
                e.title()
            );
        }
        if range.end < first.end {
            let after = archive.entry_by_title_position(range.end).unwrap();
            assert!(!after.title().starts_with(&prefix));
        }
    }
}

#[test]
fn small_zim_all_flavours() {
    let Some(dir) = common::fixtures_dir() else {
        return;
    };
    for flavour in FLAVOURS {
        let path = dir.join(flavour).join("small.zim");
        let archive = Archive::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        eprintln!("{flavour}/small.zim: {archive:?}");
        exercise(&archive);
        match flavour {
            "withns" => {
                assert!(!archive.uses_new_namespace_scheme());
                assert_eq!(archive.content_namespace(), b'A');
                assert!(matches!(archive.title_index(), TitleIndex::Header { .. }));
            }
            "nons" => {
                assert!(archive.uses_new_namespace_scheme());
                assert!(archive.header().has_title_pointer_list());
                assert!(matches!(
                    archive.title_index(),
                    TitleIndex::FrontArticles { .. }
                ));
            }
            _ => {
                assert!(archive.uses_new_namespace_scheme());
                assert!(!archive.header().has_title_pointer_list());
                assert!(matches!(
                    archive.title_index(),
                    TitleIndex::FrontArticles { .. }
                ));
            }
        }
        // metadata and compat path lookup
        assert!(
            archive.metadata_string("Title").unwrap().is_some()
                || !archive.metadata_keys().unwrap().is_empty()
        );
        let main = archive
            .resolve(&archive.main_entry().unwrap().unwrap())
            .unwrap();
        let by_compat = archive
            .entry_by_path_compat(&main.path)
            .unwrap()
            .expect("compat lookup");
        assert_eq!(archive.resolve(&by_compat).unwrap().index, main.index);
        let by_long = archive
            .entry_by_long_path(&main.full_path())
            .unwrap()
            .expect("long path lookup");
        assert_eq!(by_long.index, main.index);
    }
}

#[test]
fn wikibooks_split_matches_whole() {
    let Some(dir) = common::fixtures_dir() else {
        return;
    };
    for flavour in FLAVOURS {
        let whole =
            Archive::open(dir.join(flavour).join("wikibooks_be_all_nopic_2017-02.zim")).unwrap();
        let split = Archive::open(
            dir.join(flavour)
                .join("wikibooks_be_all_nopic_2017-02_splitted.zimaa"),
        )
        .unwrap();
        let via_base = Archive::open(
            dir.join(flavour)
                .join("wikibooks_be_all_nopic_2017-02_splitted.zim"),
        )
        .unwrap();
        assert_eq!(whole.uuid(), split.uuid());
        assert_eq!(whole.uuid(), via_base.uuid());
        assert_eq!(whole.size(), split.size());
        exercise(&whole);
        exercise(&split);
        for (a, b) in whole.entries().zip(split.entries()) {
            let (a, b) = (a.unwrap(), b.unwrap());
            assert_eq!(*a, *b);
            if a.is_item() {
                assert_eq!(
                    &*whole.item_data(&a).unwrap(),
                    &*split.item_data(&b).unwrap()
                );
            }
        }
    }
}

#[test]
fn wikipedia_climate_change() {
    let Some(dir) = common::fixtures_dir() else {
        return;
    };
    for flavour in FLAVOURS {
        let archive = Archive::open(
            dir.join(flavour)
                .join("wikipedia_en_climate_change_mini_2024-06.zim"),
        )
        .unwrap();
        exercise(&archive);
        assert!(archive.has_fulltext_index(), "{flavour}: fulltext index");
        assert!(archive.has_title_xapian_index(), "{flavour}: title index");
        let ft = archive.fulltext_index().unwrap().unwrap();
        let head = archive.source().slice(ft.offset, 16).unwrap();
        assert_eq!(&head[..14], b"\x0f\x0dXapian Glass");
        assert!(archive.article_count().unwrap() > 0);
        assert!(!archive.counter().unwrap().is_empty() || flavour == "withns");
    }
}

fn find_magic(bytes: &[u8]) -> Option<usize> {
    let magic = MAGIC.to_le_bytes();
    bytes.windows(4).position(|w| w == magic)
}

#[test]
fn embedded_archives() {
    let Some(dir) = common::fixtures_dir() else {
        return;
    };
    for flavour in FLAVOURS {
        let plain = std::fs::read(dir.join(flavour).join("small.zim")).unwrap();
        let reference = Archive::open(dir.join(flavour).join("small.zim")).unwrap();

        let path = dir.join(flavour).join("small.zim.embedded");
        let bytes = std::fs::read(&path).unwrap();
        let offset = find_magic(&bytes).expect("embedded magic");
        assert_eq!(offset, 8);
        let archive = Archive::open_embedded(
            &path,
            offset as u64,
            plain.len() as u64,
            OpenConfig::default(),
        )
        .unwrap();
        assert_eq!(archive.uuid(), reference.uuid());
        exercise(&archive);

        // libzim's own test: 2048-byte pieces separated by "NEWSECTIONZIMMULTI" markers
        let path = dir.join(flavour).join("small.zim.embedded.multi");
        let mut ranges = Vec::new();
        let mut start = "BEGINZIMMULTIPART".len() as u64;
        let mut remaining = plain.len() as u64;
        while remaining > 2048 {
            ranges.push(FileRange {
                path: path.clone(),
                offset: start,
                len: 2048,
            });
            start += 2048 + "NEWSECTIONZIMMULTI".len() as u64;
            remaining -= 2048;
        }
        ranges.push(FileRange {
            path: path.clone(),
            offset: start,
            len: remaining,
        });
        let archive = Archive::open_ranges(ranges, OpenConfig::default()).unwrap();
        assert_eq!(archive.uuid(), reference.uuid());
        exercise(&archive);
        for (a, b) in reference.entries().zip(archive.entries()) {
            let (a, b) = (a.unwrap(), b.unwrap());
            assert_eq!(*a, *b);
            if a.is_item() {
                assert_eq!(
                    &*reference.item_data(&a).unwrap(),
                    &*archive.item_data(&b).unwrap()
                );
            }
        }
    }
}

#[test]
fn invalid_archives_never_panic_and_are_detected() {
    let Some(dir) = common::fixtures_dir() else {
        return;
    };
    let mut checked = 0;
    for flavour in FLAVOURS {
        for path in common::zim_files(&dir.join(flavour)) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.starts_with("invalid.") {
                continue;
            }
            checked += 1;
            let detected = match Archive::open(&path) {
                Err(e) => {
                    eprintln!("{flavour}/{name}: open failed as expected: {e}");
                    true
                }
                Ok(archive) => {
                    let problems = integrity::run(&archive, &Check::ALL);
                    for p in &problems {
                        eprintln!("{flavour}/{name}: {:?}: {}", p.check, p.message);
                    }
                    // exercise the read paths too; errors are fine, panics are not
                    let _ = archive
                        .main_entry()
                        .and_then(|m| m.map(|m| archive.resolve(&m)).transpose());
                    for e in archive.entries().take(64).flatten() {
                        let _ = archive.item_data(&e);
                    }
                    !problems.is_empty()
                }
            };
            assert!(detected, "{flavour}/{name} was not detected as invalid");
        }
    }
    assert!(
        checked >= 60,
        "expected the invalid fixtures, found {checked}"
    );
}

#[test]
fn nonexistent_file_is_an_io_error() {
    let err = Archive::open(Path::new("/definitely/not/here.zim")).unwrap_err();
    assert!(matches!(err, zimz_core::Error::Io(_)));
}
