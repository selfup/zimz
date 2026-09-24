// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Tests against the Xapian indexes embedded in the openZIM testing-suite fixtures,
//! plus term-level comparison with `xapian-delve` when it is installed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use zimz_glass::search::{Op, Query, search};
use zimz_glass::{Analyzer, GlassDb};

const FLAVOURS: [&str; 3] = ["withns", "nons", "noTitleListingV0"];
const FIXTURE: &str = "wikipedia_en_climate_change_mini_2024-06.zim";

fn fixtures_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/zim-testing-suite/data");
    if dir.is_dir() {
        Some(dir)
    } else {
        eprintln!("skipping: fixtures not found (run scripts/fetch-fixtures.sh)");
        None
    }
}

/// Bytes of an embedded index of a fixture archive.
fn index_bytes(flavour: &str, kind: &str) -> Option<Vec<u8>> {
    let path = fixtures_dir()?.join(flavour).join(FIXTURE);
    let archive = zimz_core::Archive::open(&path).unwrap();
    let da = match kind {
        "fulltext" => archive.fulltext_index().unwrap(),
        _ => archive.title_xapian_index().unwrap(),
    }
    .unwrap_or_else(|| panic!("{flavour}: no {kind} index"));
    Some(
        archive
            .source()
            .slice(da.offset, da.len as usize)
            .unwrap()
            .into_owned(),
    )
}

fn delve() -> Option<PathBuf> {
    let out = Command::new("which").arg("xapian-delve").output().ok()?;
    if !out.status.success() {
        eprintln!("skipping oracle comparison: xapian-delve not installed (brew install xapian)");
        return None;
    }
    Some(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn temp_db(name: &str, bytes: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("zimz-glass-{}-{name}.glass", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn opens_fulltext_indexes_of_every_flavour() {
    let Some(_) = fixtures_dir() else { return };
    for flavour in FLAVOURS {
        let bytes = index_bytes(flavour, "fulltext").unwrap();
        let db = GlassDb::open(&bytes).unwrap_or_else(|e| panic!("{flavour}: {e}"));
        assert!(db.doc_count() > 0, "{flavour}");
        assert!(db.last_docid() >= db.doc_count());
        assert!(db.total_doclen() > 0);
        assert!(db.avg_doclen() > 1.0);
        assert_eq!(db.version().block_size(), 8192);
        assert_eq!(db.version().uuid_string().len(), 36);
        assert!(
            !db.has_positions(),
            "{flavour}: fulltext indexes have no positions"
        );
        // current libzim writes DB_NO_TERMLIST; older archives (withns) do have one
        eprintln!("{flavour}: termlist table present = {}", db.has_termlist());
        assert_eq!(
            db.metadata_string("kind").unwrap().as_deref(),
            Some("fulltext")
        );
        let map = db.valuesmap().unwrap();
        assert_eq!(map.get("title"), Some(&0));
        assert_eq!(map.get("wordcount"), Some(&1));
        assert_eq!(db.value_slot("geo.position").unwrap(), Some(2));
        assert!(db.metadata("does-not-exist").unwrap().is_none());
        eprintln!(
            "{flavour}: {} docs, avg len {:.1}, language {:?}",
            db.doc_count(),
            db.avg_doclen(),
            db.metadata_string("language").unwrap()
        );
    }
}

#[test]
fn title_index_has_positions() {
    let Some(bytes) = index_bytes("nons", "title") else {
        return;
    };
    let db = GlassDb::open(&bytes).unwrap();
    assert!(db.has_positions());
    assert_eq!(
        db.metadata_string("kind").unwrap().as_deref(),
        Some("title")
    );
    assert!(db.doc_count() > 0);
    // titles are indexed with the anchor term first
    assert!(db.postlist("0posanchor").unwrap().is_some());
}

#[test]
fn document_lengths_cover_every_document() {
    let Some(bytes) = index_bytes("nons", "fulltext") else {
        return;
    };
    let db = GlassDb::open(&bytes).unwrap();
    let mut pl = db.doc_lengths().unwrap().expect("doclen list");
    // Xapian writes 0/0 statistics for the length list; the real counts live in the stats
    let mut count = 0u32;
    let mut sum = 0u64;
    let mut prev = 0u32;
    let mut lens = BTreeMap::new();
    loop {
        assert!(pl.docid() > prev, "docids must increase");
        prev = pl.docid();
        count += 1;
        sum += u64::from(pl.wdf());
        lens.insert(pl.docid(), pl.wdf());
        if !pl.advance().unwrap() {
            break;
        }
    }
    assert_eq!(count, db.doc_count());
    assert_eq!(sum, db.total_doclen());
    assert_eq!(prev, db.last_docid());
    for (&did, &len) in lens.iter().step_by((lens.len() / 25).max(1)) {
        assert_eq!(db.doc_length(did).unwrap(), Some(len), "doc_length({did})");
    }
    assert_eq!(db.doc_length(db.last_docid() + 1).unwrap(), None);
    assert_eq!(db.doc_length(0).unwrap(), None);
}

#[test]
fn docdata_and_values() {
    let Some(_) = fixtures_dir() else { return };
    for flavour in FLAVOURS {
        let bytes = index_bytes(flavour, "fulltext").unwrap();
        let db = GlassDb::open(&bytes).unwrap();
        let title_slot = db.value_slot("title").unwrap().unwrap();
        let wc_slot = db.value_slot("wordcount").unwrap().unwrap();
        for did in (1..=db.last_docid()).step_by((db.last_docid() / 30).max(1) as usize) {
            let data = db
                .docdata_string(did)
                .unwrap()
                .unwrap_or_else(|| panic!("{flavour}: docdata {did}"));
            assert!(
                data.contains('/'),
                "{flavour}: docdata {did} = {data:?} should be a namespaced path"
            );
            let title = db
                .value_string(did, title_slot)
                .unwrap()
                .unwrap_or_else(|| panic!("{flavour}: title of {did}"));
            assert!(!title.is_empty());
            let wc = db
                .value_string(did, wc_slot)
                .unwrap()
                .unwrap_or_else(|| panic!("{flavour}: wordcount of {did}"));
            assert!(wc.parse::<u32>().is_ok(), "{flavour}: wordcount {wc:?}");
        }
        assert!(db.docdata(db.last_docid() + 1).unwrap().is_none());
        assert!(db.docdata(0).unwrap().is_none());
        assert!(db.value(1, 99).unwrap().is_none());
        assert!(db.value(db.last_docid() + 1, title_slot).unwrap().is_none());
        let stats = db.value_stats(title_slot).unwrap().expect("value stats");
        assert_eq!(stats.freq, db.doc_count());
        assert!(stats.lower <= stats.upper);
    }
}

#[test]
fn terms_and_postlists_are_consistent() {
    let Some(bytes) = index_bytes("noTitleListingV0", "fulltext") else {
        return;
    };
    let db = GlassDb::open(&bytes).unwrap();
    let all: Vec<_> = db.terms(b"").map(|t| t.unwrap()).collect();
    assert!(all.len() > 1000, "{} terms", all.len());
    for w in all.windows(2) {
        assert!(
            w[0].term < w[1].term,
            "terms must be sorted and unique: {:?} {:?}",
            w[0].term,
            w[1].term
        );
    }
    for t in &all {
        assert!(!t.term.is_empty() && t.term_freq >= 1 && t.coll_freq >= u64::from(t.term_freq));
    }
    // prefix iteration is a filter of the full listing
    let with_c: Vec<_> = db.terms(b"c").map(|t| t.unwrap()).collect();
    let expected: Vec<_> = all
        .iter()
        .filter(|t| t.term.starts_with(b"c"))
        .cloned()
        .collect();
    assert_eq!(with_c, expected);
    assert!(db.terms(b"\xff\xffnope").next().is_none());

    // posting lists: exact statistics and monotonic docids, plus skip_to semantics
    let mut checked = 0;
    let sample = all.iter().step_by((all.len() / 60).max(1)).chain(
        all.iter()
            .filter(|t| t.term_freq > db.doc_count() / 2)
            .take(5),
    );
    for t in sample {
        let term = String::from_utf8_lossy(&t.term).into_owned();
        let mut pl = db
            .postlist(&term)
            .unwrap()
            .unwrap_or_else(|| panic!("postlist for {term:?}"));
        assert_eq!(pl.term_freq(), t.term_freq);
        assert_eq!(pl.coll_freq(), t.coll_freq);
        let mut docids = Vec::new();
        let mut cf = 0u64;
        loop {
            assert!(docids.last().is_none_or(|&d| d < pl.docid()));
            assert!(pl.docid() >= 1 && pl.docid() <= db.last_docid());
            assert!(pl.wdf() >= 1);
            docids.push(pl.docid());
            cf += u64::from(pl.wdf());
            if !pl.advance().unwrap() {
                break;
            }
        }
        assert_eq!(docids.len() as u32, t.term_freq, "{term:?}");
        assert_eq!(cf, t.coll_freq, "{term:?}");
        for &d in docids.iter().step_by((docids.len() / 7).max(1)) {
            let mut fresh = db.postlist(&term).unwrap().unwrap();
            assert!(
                fresh.skip_to(d).unwrap() && fresh.docid() == d,
                "{term:?}: skip_to({d})"
            );
            let after = docids.iter().find(|&&x| x > d).copied();
            let mut fresh = db.postlist(&term).unwrap().unwrap();
            let ok = fresh.skip_to(d + 1).unwrap();
            assert_eq!(
                ok.then(|| fresh.docid()),
                after,
                "{term:?}: skip_to({})",
                d + 1
            );
        }
        let mut fresh = db.postlist(&term).unwrap().unwrap();
        assert!(!fresh.skip_to(db.last_docid() + 5).unwrap());
        assert!(fresh.at_end());
        checked += 1;
    }
    assert!(checked >= 40);
    assert!(db.postlist("zzzz-no-such-term").unwrap().is_none());
    assert!(db.term_freqs("zzzz-no-such-term").unwrap().is_none());
    assert!(db.postlist("").is_err());
}

fn postlist_docids(db: &GlassDb<'_>, term: &str) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    if let Some(mut pl) = db.postlist(term).unwrap() {
        loop {
            out.insert(pl.docid());
            if !pl.advance().unwrap() {
                break;
            }
        }
    }
    out
}

#[test]
#[allow(clippy::too_many_lines)]
fn search_and_or_match_set_algebra() {
    let Some(bytes) = index_bytes("nons", "fulltext") else {
        return;
    };
    let db = GlassDb::open(&bytes).unwrap();
    let n = db.doc_count();
    let mut mid: Vec<_> = db
        .terms(b"")
        .map(|t| t.unwrap())
        .filter(|t| t.term_freq > n / 8 && t.term_freq < n / 2)
        .collect();
    assert!(mid.len() >= 2, "need two mid-frequency terms");
    mid.sort_by_key(|t| t.term.clone());
    let a = String::from_utf8_lossy(&mid[0].term).into_owned();
    let b = String::from_utf8_lossy(&mid[mid.len() / 2].term).into_owned();
    let sa = postlist_docids(&db, &a);
    let sb = postlist_docids(&db, &b);

    let and = search(
        &db,
        &Query::from_terms([a.clone(), b.clone()], Op::And),
        0,
        1000,
    )
    .unwrap();
    assert_eq!(
        and.total as usize,
        sa.intersection(&sb).count(),
        "AND {a:?} {b:?}"
    );
    let got: BTreeSet<u32> = and.hits.iter().map(|h| h.docid).collect();
    assert_eq!(got, sa.intersection(&sb).copied().collect::<BTreeSet<_>>());

    let or = search(
        &db,
        &Query::from_terms([a.clone(), b.clone()], Op::Or),
        0,
        10_000,
    )
    .unwrap();
    assert_eq!(or.total as usize, sa.union(&sb).count(), "OR {a:?} {b:?}");
    let got: BTreeSet<u32> = or.hits.iter().map(|h| h.docid).collect();
    assert_eq!(got, sa.union(&sb).copied().collect::<BTreeSet<_>>());
    assert!(
        or.hits.iter().any(|h| h.matched_terms == 2)
            && or.hits.iter().any(|h| h.matched_terms == 1)
    );

    for r in [&and, &or] {
        for w in r.hits.windows(2) {
            assert!(
                w[0].weight > w[1].weight
                    || (w[0].weight.to_bits() == w[1].weight.to_bits() && w[0].docid < w[1].docid),
                "order"
            );
        }
        assert_eq!(r.hits[0].percent, 100);
        assert!(r.hits.iter().all(|h| (1..=100).contains(&h.percent)));
        assert_eq!(r.max_weight.to_bits(), r.hits[0].weight.to_bits());
    }
    // paging
    let page1 = search(
        &db,
        &Query::from_terms([a.clone(), b.clone()], Op::Or),
        0,
        5,
    )
    .unwrap();
    let page2 = search(
        &db,
        &Query::from_terms([a.clone(), b.clone()], Op::Or),
        5,
        5,
    )
    .unwrap();
    assert_eq!(page1.hits.len(), 5);
    assert_eq!(page2.total, or.total);
    assert_eq!(
        page1
            .hits
            .iter()
            .chain(&page2.hits)
            .map(|h| h.docid)
            .collect::<Vec<_>>(),
        or.hits.iter().take(10).map(|h| h.docid).collect::<Vec<_>>()
    );
    // single term: hit set is the posting list; absent term
    let single = search(&db, &Query::from_terms([a.clone()], Op::And), 0, 100_000).unwrap();
    assert_eq!(single.total as usize, sa.len());
    assert_eq!(
        search(
            &db,
            &Query::from_terms(["zzzz-absent".to_string(), a.clone()], Op::And),
            0,
            10
        )
        .unwrap()
        .total,
        0
    );
    assert_eq!(
        search(
            &db,
            &Query::from_terms(["zzzz-absent".to_string(), a.clone()], Op::Or),
            0,
            10
        )
        .unwrap()
        .total as usize,
        sa.len()
    );
    assert!(
        search(
            &db,
            &Query::from_terms(Vec::<String>::new(), Op::And),
            0,
            10
        )
        .unwrap()
        .hits
        .is_empty()
    );
}

#[test]
fn analyzer_terms_exist_in_the_index() {
    let Some(bytes) = index_bytes("nons", "fulltext") else {
        return;
    };
    let db = GlassDb::open(&bytes).unwrap();
    let analyzer = Analyzer::new(db.metadata_string("language").unwrap().as_deref());
    // the fixture is about climate change: every stem of a plain query must be indexed
    for text in [
        "Climate change",
        "global warming temperature",
        "Carbon dioxide emissions",
        "The Greenhouse Effect",
    ] {
        let q = Query::parse(&analyzer, text, Op::And);
        for t in &q.terms {
            assert!(
                db.postlist(&t.term).unwrap().is_some(),
                "{text:?} → term {:?} missing from index",
                t.term
            );
        }
        let r = search(&db, &q, 0, 5).unwrap();
        assert!(r.total > 0, "{text:?}");
        let title_slot = db.value_slot("title").unwrap().unwrap();
        let title = db
            .value_string(r.hits[0].docid, title_slot)
            .unwrap()
            .unwrap();
        eprintln!(
            "{text:?} → {} hits, top: {title:?} ({}%)",
            r.total, r.hits[0].percent
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn matches_xapian_delve() {
    let Some(delve) = delve() else { return };
    let Some(bytes) = index_bytes("noTitleListingV0", "fulltext") else {
        return;
    };
    let db = GlassDb::open(&bytes).unwrap();
    let path = temp_db("climate", &bytes);
    let all: Vec<_> = db.terms(b"").map(|t| t.unwrap()).collect();
    let mut sample: Vec<Vec<u8>> = all
        .iter()
        .step_by((all.len() / 12).max(1))
        .map(|t| t.term.clone())
        .collect();
    sample.extend(
        all.iter()
            .filter(|t| t.term_freq > db.doc_count() / 3)
            .take(3)
            .map(|t| t.term.clone()),
    );
    let mut compared = 0;
    for term in &sample {
        let Ok(term) = std::str::from_utf8(term) else {
            continue;
        };
        let out = Command::new(&delve)
            .args(["-1", "-v", "-t", term])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines();
        let header = lines.next().unwrap_or_default();
        assert!(
            header.contains("Posting List"),
            "delve output for {term:?}: {text}"
        );
        let tf: u32 = header
            .split("termfreq ")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .unwrap()
            .parse()
            .unwrap();
        let cf: u64 = header
            .split("collfreq ")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .unwrap()
            .parse()
            .unwrap();
        let expected: Vec<(u32, u32, u32)> = lines
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let mut it = l.split_whitespace().map(|x| x.parse::<u32>().unwrap());
                (it.next().unwrap(), it.next().unwrap(), it.next().unwrap())
            })
            .collect();
        let mut pl = db
            .postlist(term)
            .unwrap()
            .unwrap_or_else(|| panic!("{term:?} missing"));
        assert_eq!((pl.term_freq(), pl.coll_freq()), (tf, cf), "{term:?} stats");
        let mut ours = Vec::new();
        loop {
            ours.push((
                pl.docid(),
                pl.wdf(),
                db.doc_length(pl.docid()).unwrap().unwrap(),
            ));
            if !pl.advance().unwrap() {
                break;
            }
        }
        assert_eq!(ours, expected, "postings of {term:?}");
        compared += 1;
    }
    assert!(compared >= 10);
    // prefix listing with term frequencies
    for prefix in ["cl", "temp", "z"] {
        let out = Command::new(&delve)
            .args(["-1", "-v", "-A", prefix])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let expected: Vec<(String, u32)> = text
            .lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let (t, f) = l.rsplit_once(' ').unwrap();
                (t.to_string(), f.parse().unwrap())
            })
            .collect();
        let ours: Vec<(String, u32)> = db
            .terms(prefix.as_bytes())
            .map(|t| t.unwrap())
            .map(|t| (String::from_utf8_lossy(&t.term).into_owned(), t.term_freq))
            .collect();
        assert_eq!(ours, expected, "terms with prefix {prefix:?}");
    }
    // document data
    for did in [1u32, 2, db.last_docid() / 2, db.last_docid()] {
        let out = Command::new(&delve)
            .args(["-r", &did.to_string(), "-d"])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let expected = text.lines().nth(1).unwrap_or_default().to_string();
        assert_eq!(
            db.docdata_string(did).unwrap().unwrap(),
            expected,
            "docdata {did}"
        );
    }
    let _ = std::fs::remove_file(&path);
}
