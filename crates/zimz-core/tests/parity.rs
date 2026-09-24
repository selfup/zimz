// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Compare against a manifest produced by python-libzim (`scripts/parity.py`).
//! Enable with `ZIMZ_PARITY_MANIFEST=/path/to/manifest.json`.

mod common;

use serde::Deserialize;
use zimz_core::Archive;

#[derive(Deserialize)]
struct Manifest {
    zim: String,
    uuid: String,
    all_entry_count: u32,
    main_path: Option<String>,
    metadata: Vec<(String, String)>,
    entries: Vec<Sample>,
    title_lookups: Vec<TitleLookup>,
}

#[derive(Deserialize)]
struct Sample {
    index: u32,
    path: String,
    title: String,
    is_redirect: bool,
    mimetype: Option<String>,
    size: Option<u64>,
    md5: Option<String>,
    redirect_path: Option<String>,
}

#[derive(Deserialize)]
struct TitleLookup {
    title: String,
    path: String,
}

#[test]
fn matches_python_libzim() {
    let Some(manifest) = std::env::var_os("ZIMZ_PARITY_MANIFEST") else {
        eprintln!("skipping: ZIMZ_PARITY_MANIFEST not set");
        return;
    };
    // cargo runs integration tests from the crate directory; accept paths relative to the
    // workspace root too (as used by CI and the docs)
    let mut path = std::path::PathBuf::from(&manifest);
    if !path.exists() {
        path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(&manifest);
    }
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap();
    let archive = Archive::open(&manifest.zim).unwrap();
    assert_eq!(archive.uuid(), manifest.uuid);
    assert_eq!(archive.entry_count(), manifest.all_entry_count);
    if let Some(main) = &manifest.main_path {
        let ours = archive
            .resolve(&archive.main_entry().unwrap().unwrap())
            .unwrap();
        assert_eq!(&archive.user_path(&ours), main);
    }
    for (key, value) in &manifest.metadata {
        assert_eq!(
            archive.metadata_string(key).unwrap().as_deref(),
            Some(value.as_str()),
            "metadata {key}"
        );
    }
    let mut compared = 0;
    for s in &manifest.entries {
        let e = archive.entry(s.index).unwrap();
        assert_eq!(archive.user_path(&e), s.path, "path of entry {}", s.index);
        assert_eq!(e.title(), s.title, "title of entry {}", s.index);
        assert_eq!(e.is_redirect(), s.is_redirect, "kind of entry {}", s.index);
        if let Some(target) = &s.redirect_path {
            let r = archive.resolve(&e).unwrap();
            assert_eq!(
                &archive.user_path(&r),
                target,
                "redirect target of {}",
                s.index
            );
        }
        if e.is_item() {
            let data = archive.item_data(&e).unwrap();
            assert_eq!(Some(data.len() as u64), s.size, "size of {}", e.full_path());
            if s.md5.is_some() {
                assert_eq!(
                    Some(common::md5_hex(&data)),
                    s.md5,
                    "content of {}",
                    e.full_path()
                );
            }
            assert_eq!(
                archive.mime_type(&e).map(str::to_string),
                s.mimetype,
                "mimetype of {}",
                e.full_path()
            );
            compared += 1;
        }
    }
    for l in &manifest.title_lookups {
        let found = archive.entry_by_title(&l.title).unwrap();
        assert_eq!(
            found.map(|e| archive.user_path(&e)),
            Some(l.path.clone()),
            "title lookup {:?}",
            l.title
        );
    }
    eprintln!(
        "parity ok: {} entries, {compared} items, {} title lookups",
        manifest.entries.len(),
        manifest.title_lookups.len()
    );
}
