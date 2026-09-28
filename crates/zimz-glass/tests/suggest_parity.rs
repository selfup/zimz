// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Suggestion parity against python-libzim's `SuggestionSearcher`
//! (`scripts/suggest_parity.py`). Enable with `ZIMZ_SUGGEST_PARITY=<manifest.json>`.

use serde::Deserialize;
use zimz_glass::suggest::suggest;
use zimz_glass::{Analyzer, GlassDb};

#[derive(Deserialize)]
struct Manifest {
    zim: String,
    new_scheme: bool,
    top: usize,
    queries: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    query: String,
    estimated: u64,
    paths: Vec<String>,
}

#[test]
#[allow(clippy::cast_precision_loss)]
fn suggestions_match_python_libzim() {
    let Some(manifest) = std::env::var_os("ZIMZ_SUGGEST_PARITY") else {
        eprintln!("skipping: ZIMZ_SUGGEST_PARITY not set");
        return;
    };
    let mut path = std::path::PathBuf::from(&manifest);
    if !path.exists() {
        path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(&manifest);
    }
    let m: Manifest = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let archive = zimz_core::Archive::open(&m.zim).unwrap();
    let da = archive.title_xapian_index().unwrap().expect("title index");
    let bytes = archive.source().slice(da.offset, da.len as usize).unwrap();
    let db = GlassDb::open(&bytes).unwrap();
    let language = db
        .metadata_string("language")
        .unwrap()
        .or_else(|| archive.metadata_string("Language").unwrap());
    let analyzer = Analyzer::new(language.as_deref());

    let (mut overlap_sum, mut top1, mut exact) = (0.0, 0usize, 0usize);
    for case in &m.queries {
        let t = std::time::Instant::now();
        let r = suggest(&db, &analyzer, &case.query, 0, m.top).unwrap();
        let took = t.elapsed();
        let ours: Vec<String> = r
            .hits
            .iter()
            .map(|h| {
                if m.new_scheme {
                    h.path.strip_prefix("C/").unwrap_or(&h.path).to_string()
                } else {
                    h.path.clone()
                }
            })
            .collect();
        let theirs = &case.paths;
        let k = theirs.len().max(ours.len()).max(1);
        let common = ours.iter().filter(|p| theirs.contains(p)).count();
        let overlap = if theirs.is_empty() && ours.is_empty() {
            1.0
        } else {
            common as f64 / k as f64
        };
        overlap_sum += overlap;
        top1 += usize::from(ours.first() == theirs.first());
        exact += usize::from(ours == *theirs);
        if ours != *theirs {
            eprintln!(
                "{:?}: overlap {overlap:.2} (total {} vs est {}) in {took:.1?}\n   ours:   {ours:?}\n   libzim: {theirs:?}",
                case.query, r.total, case.estimated
            );
        }
    }
    let n = m.queries.len();
    eprintln!(
        "suggest parity on {}: {n} queries; mean top-{} overlap {:.3}; top-1 agreement {top1}/{n}; identical order {exact}/{n}",
        std::path::Path::new(&m.zim)
            .file_name()
            .unwrap()
            .to_string_lossy(),
        m.top,
        overlap_sum / n as f64
    );
    assert!(overlap_sum / n as f64 >= 0.9, "mean overlap too low");
    assert!(top1 * 10 >= n * 8, "top-1 agreement too low: {top1}/{n}");
}
