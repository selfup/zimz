// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Ranking parity against python-libzim's `Searcher` (`scripts/search_parity.py`).
//! Enable with `ZIMZ_SEARCH_PARITY=<manifest.json>`.

use serde::Deserialize;
use zimz_glass::search::{Op, Query, search};
use zimz_glass::{Analyzer, GlassDb};

#[derive(Deserialize)]
struct Manifest {
    zim: String,
    new_scheme: bool,
    top: usize,
    queries: Vec<QueryCase>,
}

#[derive(Deserialize)]
struct QueryCase {
    query: String,
    estimated: u64,
    paths: Vec<String>,
}

#[test]
#[allow(clippy::cast_precision_loss)]
fn ranking_matches_python_libzim() {
    let Some(manifest) = std::env::var_os("ZIMZ_SEARCH_PARITY") else {
        eprintln!("skipping: ZIMZ_SEARCH_PARITY not set");
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
    let da = archive.fulltext_index().unwrap().expect("fulltext index");
    let bytes = archive.source().slice(da.offset, da.len as usize).unwrap();
    let db = GlassDb::open(&bytes).unwrap();
    let language = db
        .metadata_string("language")
        .unwrap()
        .or_else(|| archive.metadata_string("Language").unwrap());
    let analyzer = Analyzer::new(language.as_deref());

    let mut overlap_sum = 0.0;
    let mut top1 = 0usize;
    let mut exact_order = 0usize;
    let mut total_ok = 0usize;
    let mut with_hits = 0usize;
    for case in &m.queries {
        let q = Query::parse(&analyzer, &case.query, Op::And);
        let r = search(&db, &q, 0, m.top).unwrap();
        let ours: Vec<String> = r
            .hits
            .iter()
            .map(|h| {
                let data = db.docdata_string(h.docid).unwrap().unwrap();
                if m.new_scheme {
                    data.strip_prefix("C/").unwrap_or(&data).to_string()
                } else {
                    data
                }
            })
            .collect();
        let theirs = &case.paths;
        if theirs.is_empty() && ours.is_empty() {
            overlap_sum += 1.0;
            top1 += 1;
            exact_order += 1;
            total_ok += usize::from(r.total == 0);
            continue;
        }
        with_hits += 1;
        let k = theirs.len().max(ours.len());
        let common = ours.iter().filter(|p| theirs.contains(p)).count();
        let overlap = common as f64 / k as f64;
        overlap_sum += overlap;
        if ours.first() == theirs.first() {
            top1 += 1;
        }
        if ours == *theirs {
            exact_order += 1;
        }
        // libzim's count is an estimate (check_at_least 10); exact for small result sets
        let est = case.estimated;
        let close = if est <= 30 {
            u64::from(r.total) == est
        } else {
            (u64::from(r.total) as f64 / est as f64 - 1.0).abs() < 0.25
        };
        total_ok += usize::from(close);
        if overlap < 1.0 || ours != *theirs || !close {
            eprintln!(
                "{:?}: overlap {overlap:.2}, exact order {}, total {} vs est {est}\n   ours:   {ours:?}\n   libzim: {theirs:?}",
                case.query,
                ours == *theirs,
                r.total
            );
        }
    }
    let n = m.queries.len();
    eprintln!(
        "search parity on {}: {n} queries; mean top-{} overlap {:.3}; top-1 agreement {top1}/{n}; identical order {exact_order}/{n}; totals close {total_ok}/{n} ({with_hits} with hits)",
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
