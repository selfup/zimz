// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! End-to-end tests over the zim-testing-suite fixtures: a library mixing an
//! old-scheme archive with both Xapian indexes, new-scheme archives with only a title
//! index, an archive with no index at all, duplicate names and a corrupt file.
#![allow(clippy::many_single_char_names)]

use std::path::{Path, PathBuf};

use zimz_search::{
    ArticleRequest, ContextRequest, Error, Format, HealthRequest, Library, LibraryConfig,
    LinksRequest, Mode, ModeSelect, SearchRequest, SuggestRequest, Verify,
};

fn fixture_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/zim-testing-suite/data");
    assert!(
        root.is_dir(),
        "fixtures missing; run scripts/fetch-fixtures.sh"
    );
    root
}

const CLIMATE: &str = "wikipedia_en_climate_change";

/// A temp directory of symlinks so file names can differ from the originals.
struct Fixture {
    _dir: tempfile::TempDir,
    library: Library,
}

fn link(dir: &Path, name: &str, target: &Path) {
    std::os::unix::fs::symlink(target, dir.join(name)).expect("symlink");
}

fn build(config: impl FnOnce(&mut LibraryConfig)) -> Fixture {
    let root = fixture_root();
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    link(
        d,
        "climate.zim",
        &root.join("withns/wikipedia_en_climate_change_mini_2024-06.zim"),
    );
    link(d, "small.zim", &root.join("nons/small.zim"));
    link(d, "small_withns.zim", &root.join("withns/small.zim"));
    link(
        d,
        "wikibooks_new.zim",
        &root.join("nons/wikibooks_be_all_nopic_2017-02.zim"),
    );
    link(d, "notes.txt", &root.join("nons/small.zim"));
    std::fs::create_dir(d.join("sub")).unwrap();
    link(
        &d.join("sub"),
        "wikibooks_old.zim",
        &root.join("withns/wikibooks_be_all_nopic_2017-02.zim"),
    );
    link(
        &d.join("sub"),
        "broken.zim",
        &root.join("nons/invalid.smaller_than_header.zim"),
    );
    let mut cfg = LibraryConfig {
        dirs: vec![d.to_path_buf()],
        ..LibraryConfig::default()
    };
    config(&mut cfg);
    let library = Library::scan(cfg).expect("scan");
    Fixture { _dir: dir, library }
}

fn lib() -> Fixture {
    build(|_| {})
}

#[test]
fn scan_builds_a_catalogue_with_unique_names() {
    let f = lib();
    let lib = &f.library;
    let mut names: Vec<&str> = lib.archives().map(|a| a.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "kiwix.wikibooks_be_all",
            "small",
            "small_withns",
            "wikibooks_new",
            CLIMATE,
        ],
        "metadata names first (scan order), file stems for missing or duplicate names"
    );
    assert_eq!(lib.failures().len(), 1);
    assert!(lib.failures()[0].file.ends_with("broken.zim"));

    let c = lib.get(CLIMATE).unwrap();
    let info = &c.info;
    assert_eq!(info.language.as_deref(), Some("eng"));
    assert_eq!(info.title.as_deref(), Some("Climate change by Wikipedia"));
    assert!(info.has_fulltext_index && info.has_title_index);
    assert_eq!(info.search_mode, Some(Mode::Fulltext));
    assert_eq!(info.article_count, 3821);
    assert!(
        info.fulltext_docs.unwrap() > 3000,
        "{:?}",
        info.fulltext_docs
    );
    assert_eq!(info.adapter, "MwOffliner");
    assert_eq!(info.main_page.as_deref(), Some("A/index"));
    assert_eq!(info.zim_version, "5.0");
    assert!(info.size_bytes > 10 << 20);

    assert_eq!(
        lib.get("small").unwrap().info.search_mode,
        Some(Mode::Title)
    );
    assert_eq!(
        lib.get("wikibooks_new").unwrap().info.search_mode,
        Some(Mode::Title)
    );
    assert_eq!(
        lib.get("kiwix.wikibooks_be_all").unwrap().info.search_mode,
        Some(Mode::Listing),
        "sub/wikibooks_old.zim scans first and takes the metadata name"
    );
    assert_eq!(
        lib.get("small_withns").unwrap().info.search_mode,
        Some(Mode::Fulltext)
    );
    // uuid and file stem also resolve
    assert!(lib.get(&info.uuid.clone()).is_ok());
    assert!(lib.get("climate").is_ok());
    assert!(matches!(lib.get("nope"), Err(Error::NoSuchArchive(_))));
}

#[test]
fn non_recursive_scan_skips_subdirectories() {
    let f = build(|c| c.recursive = false);
    assert_eq!(f.library.len(), 4);
    assert!(f.library.failures().is_empty());
}

#[test]
fn explicit_files_and_priorities() {
    let root = fixture_root();
    let lib = Library::scan(LibraryConfig {
        files: vec![root.join("withns/wikipedia_en_climate_change_mini_2024-06.zim")],
        priorities: vec![("wikipedia_*".into(), 2.5)],
        ..LibraryConfig::default()
    })
    .unwrap();
    assert_eq!(lib.len(), 1);
    assert!((lib.get(CLIMATE).unwrap().info.priority - 2.5).abs() < 1e-9);
}

#[test]
fn select_by_name_glob_and_list() {
    let f = lib();
    let lib = &f.library;
    let all = lib.select(&[]).unwrap();
    assert_eq!(all.len(), 5);
    let wiki = lib.select(&["wiki*".into()]).unwrap();
    assert_eq!(
        wiki.len(),
        3,
        "climate, wikibooks_new and the file stem wikibooks_old"
    );
    let two = lib
        .select(&["small, kiwix.wikibooks_be_all".into()])
        .unwrap();
    assert_eq!(two.len(), 2);
    assert!(matches!(lib.select(&["zzz".into()]), Err(Error::NoSuchArchive(p)) if p == "zzz"));
    assert_eq!(lib.list(Some("climate")).len(), 1);
    assert_eq!(lib.list(Some("bel")).len(), 2, "language filter");
    assert_eq!(lib.list(None).len(), 5);
}

#[test]
fn fulltext_search_ranks_exact_title_first_and_snippets() {
    let f = lib();
    let mut req = SearchRequest::new("greenhouse gas emissions");
    req.archives = vec![CLIMATE.into()];
    let res = f.library.search(&req).unwrap();
    assert!(res.total_estimate > 400, "{}", res.total_estimate);
    assert_eq!(res.hits.len(), 10);
    assert!(!res.fallback_used);
    assert_eq!(res.archives_searched, 1);
    let top = &res.hits[0];
    assert_eq!(
        top.title, "Greenhouse gas emissions",
        "exact title match is boosted to the top"
    );
    assert_eq!(top.path, "A/Greenhouse_gas_emissions");
    assert_eq!(
        top.uri,
        format!("zim://{CLIMATE}/A/Greenhouse_gas_emissions")
    );
    assert_eq!(top.mode, Mode::Fulltext);
    assert!((top.score - 1.0).abs() < 1e-9);
    assert!(top.percent >= 90);
    assert!(top.word_count.unwrap() > 100);
    assert!(!top.partial);
    let snippet = top.snippet.as_deref().unwrap();
    assert!(snippet.to_lowercase().contains("**greenhouse"), "{snippet}");
    assert!(
        snippet.chars().count() <= 300,
        "{}",
        snippet.chars().count()
    );
    for w in res.hits.windows(2) {
        assert!(w[0].score >= w[1].score);
        assert_eq!(w[1].rank, w[0].rank + 1);
    }
}

#[test]
fn search_without_snippets_is_cheap_and_searches_everything() {
    let f = lib();
    let mut req = SearchRequest::new("methane");
    req.snippet_chars = 0;
    let res = f.library.search(&req).unwrap();
    assert_eq!(res.archives_searched, 5);
    assert!(res.hits.iter().all(|h| h.snippet.is_none()));
    assert!(res.hits.iter().all(|h| h.archive == CLIMATE));
    assert!(res.hits[0].title.to_lowercase().contains("methane"));
}

#[test]
fn pagination_is_stable_and_cursor_is_validated() {
    let f = lib();
    let mut req = SearchRequest::new("carbon dioxide");
    req.limit = 5;
    req.snippet_chars = 0;
    let p1 = f.library.search(&req).unwrap();
    assert_eq!(p1.hits.len(), 5);
    let cursor = p1.next_cursor.clone().expect("more pages");
    req.cursor = Some(cursor.clone());
    let p2 = f.library.search(&req).unwrap();
    assert_eq!(p2.hits.len(), 5);
    assert_eq!(p2.hits[0].rank, 6);
    let p1_paths: Vec<&str> = p1.hits.iter().map(|h| h.path.as_str()).collect();
    assert!(p2.hits.iter().all(|h| !p1_paths.contains(&h.path.as_str())));
    // A ten-hit page equals the two five-hit pages.
    let mut ten = SearchRequest::new("carbon dioxide");
    ten.snippet_chars = 0;
    let all = f.library.search(&ten).unwrap();
    let joined: Vec<&str> = p1
        .hits
        .iter()
        .chain(&p2.hits)
        .map(|h| h.path.as_str())
        .collect();
    let direct: Vec<&str> = all.hits.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(joined, direct);
    // Cursor from a different query is rejected.
    let mut other = SearchRequest::new("methane");
    other.cursor = Some(cursor);
    assert!(matches!(f.library.search(&other), Err(Error::BadCursor)));
}

#[test]
fn or_fallback_marks_partial_hits() {
    let f = lib();
    let mut req = SearchRequest::new("methane zzzzqqqx");
    req.snippet_chars = 0;
    let res = f.library.search(&req).unwrap();
    assert!(res.fallback_used);
    assert!(!res.hits.is_empty());
    assert!(res.hits.iter().all(|h| h.partial));
    assert!(res.hits[0].score <= 1.0);
    req.or_fallback = false;
    let strict = f.library.search(&req).unwrap();
    assert!(strict.hits.is_empty());
    assert!(!strict.fallback_used);
    // AND hits come first, OR-only hits after them with lower scores.
    let mut mixed = SearchRequest::new("methane clathrate");
    mixed.snippet_chars = 0;
    mixed.limit = 50;
    let res = f.library.search(&mixed).unwrap();
    let first_partial = res.hits.iter().position(|h| h.partial);
    if let Some(p) = first_partial {
        assert!(res.hits[..p].iter().all(|h| !h.partial));
        assert!(res.hits[p..].iter().all(|h| h.partial));
        assert!(res.hits[p].score <= 0.5);
    }
}

#[test]
fn quoted_phrases_are_verified_in_the_text() {
    let f = lib();
    // Plain AND finds articles mentioning all three words anywhere; the phrase requires
    // them adjacent and in order (punctuation ignored, like Xapian positions), which
    // "removal dioxide carbon" never is.
    let mut and = SearchRequest::new("removal dioxide carbon");
    and.snippet_chars = 0;
    assert!(!f.library.search(&and).unwrap().hits.is_empty());
    let mut reversed = SearchRequest::new("\"removal dioxide carbon\"");
    reversed.snippet_chars = 0;
    let res = f.library.search(&reversed).unwrap();
    assert!(res.hits.is_empty(), "{:?}", res.hits);
    assert_eq!(res.phrases, vec!["removal dioxide carbon"]);
    // The budget warning appears exactly when candidates were left unexamined.
    assert_eq!(
        res.next_cursor.is_some(),
        res.warnings.iter().any(|w| w.contains("phrase check")),
        "{:?}",
        res.warnings
    );

    let mut req = SearchRequest::new("\"carbon dioxide removal\"");
    req.limit = 5;
    let res = f.library.search(&req).unwrap();
    assert_eq!(res.phrases, vec!["carbon dioxide removal"]);
    assert_eq!(res.hits.len(), 5);
    assert_eq!(res.hits[0].title, "Carbon dioxide removal");
    for h in &res.hits {
        let mut a = article(CLIMATE, &h.path);
        a.format = Format::Text;
        a.max_chars = 1 << 22;
        let text = f.library.read_article(&a).unwrap().content.to_lowercase();
        let title = h.title.to_lowercase();
        assert!(
            text.contains("carbon dioxide removal") || title.contains("carbon dioxide removal"),
            "{} lacks the phrase",
            h.path
        );
    }
    // Pagination keeps verifying from where the previous page stopped.
    let cursor = res.next_cursor.clone().expect("more");
    req.cursor = Some(cursor);
    let page2 = f.library.search(&req).unwrap();
    assert!(!page2.hits.is_empty());
    let p1: Vec<&str> = res.hits.iter().map(|h| h.path.as_str()).collect();
    assert!(page2.hits.iter().all(|h| !p1.contains(&h.path.as_str())));
    assert!(page2.hits[0].rank > 5);
    // Curly quotes and mixed plain terms work too, and context inherits phrases.
    let mut mixed = SearchRequest::new("\u{201c}greenhouse gas\u{201d} agriculture");
    mixed.snippet_chars = 0;
    let res = f.library.search(&mixed).unwrap();
    assert_eq!(res.phrases, vec!["greenhouse gas"]);
    assert!(!res.hits.is_empty());
    let ctx = f
        .library
        .context(&ContextRequest::new("\"greenhouse gas emissions\""))
        .unwrap();
    assert!(!ctx.excerpts.is_empty());
}

#[test]
fn min_score_filters_and_empty_query_is_rejected() {
    let f = lib();
    let mut req = SearchRequest::new("carbon");
    req.snippet_chars = 0;
    req.min_score = Some(0.99);
    let res = f.library.search(&req).unwrap();
    assert!(res.hits.iter().all(|h| h.score >= 0.99));
    assert!(res.hits.len() < 10);
    assert!(matches!(
        f.library.search(&SearchRequest::new("   ")),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn title_and_listing_modes_serve_archives_without_fulltext() {
    let f = lib();
    let mut req = SearchRequest::new("Test");
    req.archives = vec!["small".into()];
    let res = f.library.search(&req).unwrap();
    assert_eq!(res.hits.len(), 1);
    assert_eq!(res.hits[0].mode, Mode::Title);
    assert_eq!(res.hits[0].path, "main.html");
    assert_eq!(res.hits[0].title, "Test ZIM file");
    assert!(res.hits[0].snippet.is_some());

    let mut req = SearchRequest::new("італьянская");
    req.archives = vec!["kiwix.wikibooks_be_all".into()];
    let res = f.library.search(&req).unwrap();
    assert!(
        !res.hits.is_empty(),
        "capitalised prefix scan of the listing"
    );
    assert!(res.hits.iter().all(|h| h.mode == Mode::Listing));
    assert!(res.hits[0].title.starts_with("Італьянская"));

    // Forcing a mode an archive cannot serve yields a warning, not an error.
    let mut req = SearchRequest::new("Test");
    req.mode = ModeSelect::Fulltext;
    req.archives = vec!["small".into()];
    let res = f.library.search(&req).unwrap();
    assert!(res.hits.is_empty());
    assert!(
        res.warnings
            .iter()
            .any(|w| w.contains("small") && w.contains("fulltext"))
    );
}

#[test]
fn suggestions_are_federated_and_exact_titles_first() {
    let f = lib();
    let res = f
        .library
        .suggest(&SuggestRequest {
            prefix: "carbon".into(),
            archives: vec![],
            limit: 8,
        })
        .unwrap();
    assert_eq!(res.suggestions.len(), 8);
    assert!(
        res.suggestions
            .iter()
            .all(|s| s.archive == CLIMATE && s.mode == Mode::Title)
    );
    assert!(
        res.suggestions
            .iter()
            .all(|s| s.title.to_lowercase().contains("carbon"))
    );
    assert_eq!(res.archives_searched, 5);

    let res = f
        .library
        .suggest(&SuggestRequest {
            prefix: "blue carbon".into(),
            archives: vec![CLIMATE.into()],
            limit: 5,
        })
        .unwrap();
    assert_eq!(res.suggestions[0].title.to_lowercase(), "blue carbon");

    let res = f
        .library
        .suggest(&SuggestRequest {
            prefix: "Італ".into(),
            archives: vec!["kiwix.wikibooks_be_all".into()],
            limit: 5,
        })
        .unwrap();
    assert!(!res.suggestions.is_empty());
    assert_eq!(res.suggestions[0].mode, Mode::Listing);
    assert!(
        f.library
            .suggest(&SuggestRequest {
                prefix: " ".into(),
                archives: vec![],
                limit: 5
            })
            .is_err()
    );
}

fn article(archive: &str, path: &str) -> ArticleRequest {
    ArticleRequest {
        archive: archive.into(),
        path: path.into(),
        format: Format::Markdown,
        max_chars: 8000,
        offset: 0,
        section: None,
    }
}

#[test]
fn read_article_windows_sections_and_path_forms() {
    let f = lib();
    let lib = &f.library;
    let mut req = article(CLIMATE, "A/Carbon_dioxide");
    req.max_chars = 500;
    let r = lib.read_article(&req).unwrap();
    assert_eq!(r.title, "Carbon dioxide");
    assert_eq!(r.path, "A/Carbon_dioxide");
    assert!(r.truncated);
    assert_eq!(r.content.chars().count(), 500);
    assert_eq!(r.next_offset, Some(500));
    assert!(r.total_chars > 500);
    assert!(
        r.outline.as_ref().is_some_and(|o| !o.is_empty()),
        "outline comes with truncation"
    );
    assert!(r.word_count.unwrap() > 100);
    assert!(r.links_count.unwrap() > 0);
    assert_eq!(r.adapter, "MwOffliner");

    req.offset = 500;
    let r2 = lib.read_article(&req).unwrap();
    assert_eq!(r2.offset, 500);
    assert!(!r2.content.is_empty());
    assert_ne!(r2.content, r.content);

    // Whole article in one go: no outline, no next offset.
    let mut big = article(CLIMATE, "A/Carbon_dioxide");
    big.max_chars = 1 << 20;
    let whole = lib.read_article(&big).unwrap();
    assert!(!whole.truncated && whole.outline.is_none() && whole.next_offset.is_none());
    assert_eq!(whole.total_chars, whole.content.chars().count());

    // Plain text and HTML formats.
    let mut t = article(CLIMATE, "A/Carbon_dioxide");
    t.format = Format::Text;
    let t = lib.read_article(&t).unwrap();
    assert!(!t.content.contains("](zim://"));
    let mut h = article(CLIMATE, "A/Carbon_dioxide");
    h.format = Format::Html;
    let h = lib.read_article(&h).unwrap();
    assert!(h.content.contains('<'));
    assert!(h.word_count.is_none());

    // Sections by index and by title prefix ("mini" archives keep only the lead
    // section, so there is exactly one).
    let outline = lib.outline(CLIMATE, "Carbon_dioxide").unwrap();
    assert!(!outline.sections.is_empty(), "{:?}", outline.sections);
    assert_eq!(outline.sections[0].title, "Carbon dioxide");
    assert!(outline.sections[0].chars > 1000);
    let first = &outline.sections[0];
    let mut s = article(CLIMATE, "Carbon_dioxide");
    s.section = Some(first.index.to_string());
    let by_index = lib.read_article(&s).unwrap();
    assert_eq!(by_index.section.as_deref(), Some(first.title.as_str()));
    assert!(by_index.content.starts_with("# Carbon dioxide"));
    s.section = Some("carbon dio".into());
    let by_title = lib.read_article(&s).unwrap();
    assert_eq!(by_title.content, by_index.content);
    s.section = Some("no such heading".into());
    match lib.read_article(&s) {
        Err(Error::Invalid(msg)) => assert!(msg.contains("0=\"Carbon dioxide\""), "{msg}"),
        other => panic!("expected an Invalid error, got {other:?}"),
    }

    // Path forms: bare, URI, title with spaces, percent-encoded, redirect.
    for p in [
        "Carbon_dioxide",
        "A/Carbon_dioxide",
        &format!("zim://{CLIMATE}/A/Carbon_dioxide"),
        "Carbon dioxide",
        "Carbon%20dioxide",
        "A/Carbon_dioxide#Uses",
    ] {
        let r = lib
            .read_article(&article(CLIMATE, p))
            .unwrap_or_else(|e| panic!("{p}: {e}"));
        assert_eq!(r.path, "A/Carbon_dioxide", "{p}");
        assert!(r.redirected_from.is_none(), "{p}");
    }
    let r = lib
        .read_article(&article(CLIMATE, "A/Carbon-neutral"))
        .unwrap();
    assert_eq!(r.redirected_from.as_deref(), Some("A/Carbon-neutral"));
    assert_ne!(r.path, "A/Carbon-neutral");
    assert!(matches!(
        lib.read_article(&article(CLIMATE, "Definitely_missing_page")),
        Err(Error::NoSuchEntry { .. })
    ));
    assert!(matches!(
        lib.read_article(&article("nope", "x")),
        Err(Error::NoSuchArchive(_))
    ));
    // Empty path → main page.
    let main = lib.read_article(&article("small", "")).unwrap();
    assert_eq!(main.path, "main.html");
}

#[test]
fn links_resolve_targets_and_paginate() {
    let f = lib();
    let r = f
        .library
        .links(&LinksRequest {
            archive: CLIMATE.into(),
            path: "Carbon_dioxide".into(),
            limit: 5,
            offset: 0,
        })
        .unwrap();
    assert!(r.total > 5, "{}", r.total);
    assert_eq!(r.links.len(), 5);
    assert_eq!(r.next_offset, Some(5));
    assert!(
        r.links
            .iter()
            .all(|l| l.uri.starts_with(&format!("zim://{CLIMATE}/")))
    );
    let existing = r.links.iter().filter(|l| l.exists).count();
    assert!(existing >= 4, "{:?}", r.links);
    assert!(
        r.links
            .iter()
            .filter(|l| l.exists)
            .all(|l| l.title.is_some())
    );
    // Every existing link target is readable by the path we hand out.
    for l in r.links.iter().filter(|l| l.exists) {
        f.library.read_article(&article(CLIMATE, &l.path)).unwrap();
    }
}

#[test]
fn context_packs_excerpts_under_budget_with_citations() {
    let f = lib();
    let mut req = ContextRequest::new("greenhouse gas emissions agriculture");
    req.budget_chars = 3000;
    req.per_hit_chars = 600;
    req.max_hits = 4;
    let r = f.library.context(&req).unwrap();
    assert!(!r.excerpts.is_empty());
    assert!(r.excerpts.len() <= 4);
    assert!(r.chars_used <= 3000, "{}", r.chars_used);
    assert_eq!(
        r.chars_used,
        r.excerpts.iter().map(|e| e.chars).sum::<usize>()
    );
    for e in &r.excerpts {
        assert!(e.chars <= 600, "{}", e.chars);
        assert!(e.uri.starts_with(&format!("zim://{CLIMATE}/")));
        assert!(e.matched_terms >= 1, "{e:?}");
        assert!(e.score > 0.0 && e.score <= 1.0);
    }
    assert_eq!(
        r.excerpts[0].title,
        "Greenhouse gas emissions from agriculture"
    );
    let md = zimz_search::render_markdown(&r);
    assert!(md.contains("Source: zim://"));
    assert!(md.contains("## Greenhouse gas emissions from agriculture"));
    // A section-level citation carries the heading as a fragment.
    if let Some(e) = r.excerpts.iter().find(|e| e.section.is_some()) {
        assert!(e.uri.contains('#'));
    }
    assert!(r.elapsed_ms < 5_000);
}

#[test]
fn context_budget_stops_packing() {
    let f = lib();
    let mut req = ContextRequest::new("carbon");
    req.budget_chars = 700;
    req.per_hit_chars = 600;
    req.max_hits = 6;
    let r = f.library.context(&req).unwrap();
    assert!(r.chars_used <= 700);
    assert!(r.excerpts.len() <= 2);
}

#[test]
fn health_reports_indexes_caches_and_failures() {
    let f = lib();
    let lib = &f.library;
    let r = lib
        .health(&HealthRequest {
            archive: None,
            verify: Verify::None,
        })
        .unwrap();
    assert_eq!(r.archives.len(), 5);
    assert_eq!(r.failures.len(), 1);
    let c = r.archives.iter().find(|a| a.name == CLIMATE).unwrap();
    assert_eq!(c.checksum, "skipped");
    assert!(
        c.fulltext_coverage.unwrap() > 0.8,
        "{:?}",
        c.fulltext_coverage
    );
    assert!(c.fulltext_coverage.unwrap() <= 1.0);
    assert!(lib.get(CLIMATE).unwrap().info.html_count.unwrap() > 3000);
    assert!(c.open_ms >= 0.0);
    // Reading an article twice hits the extract cache.
    lib.read_article(&article(CLIMATE, "Carbon_dioxide"))
        .unwrap();
    lib.read_article(&article(CLIMATE, "Carbon_dioxide"))
        .unwrap();
    let r = lib
        .health(&HealthRequest {
            archive: Some(CLIMATE.into()),
            verify: Verify::Quick,
        })
        .unwrap();
    assert_eq!(r.archives.len(), 1);
    assert!(
        r.archives[0].problems.is_empty(),
        "{:?}",
        r.archives[0].problems
    );
    assert!(r.extract_cache.hits >= 1);
    assert!(r.extract_cache.entries >= 1);
    assert!(r.archives[0].cluster_cache.hits + r.archives[0].cluster_cache.misses > 0);
    let full = lib
        .health(&HealthRequest {
            archive: Some("small".into()),
            verify: Verify::Full,
        })
        .unwrap();
    assert_eq!(full.archives[0].checksum, "ok");
    assert!(full.archives[0].problems.is_empty());
}

#[test]
fn serde_round_trip_of_requests() {
    let v: SearchRequest = serde_json::from_str(r#"{"query":"x"}"#).unwrap();
    assert_eq!(v.limit, 10);
    assert_eq!(v.snippet_chars, 300);
    assert!(v.or_fallback);
    let v: SearchRequest =
        serde_json::from_str(r#"{"query":"x","mode":"title","limit":3}"#).unwrap();
    assert_eq!(v.mode, ModeSelect::Title);
    let v: ArticleRequest =
        serde_json::from_str(r#"{"archive":"a","path":"p","format":"html"}"#).unwrap();
    assert_eq!(v.format, Format::Html);
    assert_eq!(v.max_chars, 8000);
    let v: ContextRequest = serde_json::from_str(r#"{"query":"q"}"#).unwrap();
    assert_eq!(
        (v.budget_chars, v.per_hit_chars, v.max_hits),
        (12_000, 1_500, 6)
    );
}
