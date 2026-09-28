// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Compact text renderings of tool results: what a model reads when its client shows
//! `content` rather than `structuredContent`.

use std::fmt::Write as _;

use zimz_search::{
    ArticleResponse, ContextResponse, HealthResponse, LinksResponse, OutlineResponse,
    SearchResponse, SuggestResponse,
};

use crate::server::ListArchivesResponse;

#[allow(clippy::cast_precision_loss)]
fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn warnings(out: &mut String, warnings: &[String]) {
    for w in warnings {
        let _ = writeln!(out, "warning: {w}");
    }
}

pub fn archives(r: &ListArchivesResponse) -> String {
    let mut out = format!("{} archive(s)\n", r.count);
    for a in &r.archives {
        let mode = a
            .search_mode
            .map_or("no search".to_string(), |m| m.to_string());
        let _ = writeln!(
            out,
            "- {}: {} [{}; {}; {} articles; {}; {}]",
            a.name,
            a.title.as_deref().unwrap_or("(untitled)"),
            a.language.as_deref().unwrap_or("?"),
            a.date.as_deref().unwrap_or("undated"),
            a.article_count,
            human(a.size_bytes),
            mode
        );
        if let Some(d) = &a.description {
            let _ = writeln!(out, "    {d}");
        }
    }
    for f in &r.failures {
        let _ = writeln!(out, "failed to open {}: {}", f.file, f.error);
    }
    out
}

pub fn search(r: &SearchResponse) -> String {
    let mut out = format!(
        "{} hit(s) shown, about {} matching in {} archive(s) for {:?}{}\n",
        r.hits.len(),
        r.total_estimate,
        r.archives_searched,
        r.query,
        if r.fallback_used {
            " (AND matched too few; OR matches appended and marked partial)"
        } else {
            ""
        }
    );
    for h in &r.hits {
        let _ = writeln!(
            out,
            "{}. {} — archive: {} path: {} (score {:.2}, {}%{}{})",
            h.rank,
            h.title,
            h.archive,
            h.path,
            h.score,
            h.percent,
            if h.partial { ", partial" } else { "" },
            h.word_count
                .map(|w| format!(", {w} words"))
                .unwrap_or_default()
        );
        if let Some(s) = &h.snippet {
            let _ = writeln!(out, "   {s}");
        }
    }
    if let Some(c) = &r.next_cursor {
        let _ = writeln!(out, "next_cursor: {c}");
    }
    warnings(&mut out, &r.warnings);
    out
}

pub fn article(r: &ArticleResponse) -> String {
    let mut out = format!("# {} (archive: {}, path: {})\n", r.title, r.archive, r.path);
    if let Some(from) = &r.redirected_from {
        let _ = writeln!(out, "(redirected from {from})");
    }
    if let Some(s) = &r.section {
        let _ = writeln!(out, "(section: {s})");
    }
    out.push('\n');
    out.push_str(&r.content);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if r.truncated {
        let _ = write!(
            out,
            "\n[showing characters {}..{} of {}; continue with offset={}",
            r.offset,
            r.offset + r.content.chars().count(),
            r.total_chars,
            r.next_offset.unwrap_or(r.total_chars)
        );
        if let Some(o) = &r.outline {
            out.push_str(" or pick a section: ");
            let names: Vec<String> = o
                .iter()
                .map(|s| format!("{}={:?}", s.index, s.title))
                .collect();
            out.push_str(&names.join(", "));
        }
        out.push_str("]\n");
    }
    out
}

pub fn outline(r: &OutlineResponse) -> String {
    let mut out = format!(
        "{} (archive: {}, path: {}; {} words, {} chars)\n",
        r.title, r.archive, r.path, r.word_count, r.total_chars
    );
    for s in &r.sections {
        let _ = writeln!(
            out,
            "{:>3}. {}{} ({} chars)",
            s.index,
            "  ".repeat(usize::from(s.level.saturating_sub(1))),
            s.title,
            s.chars
        );
    }
    out
}

pub fn suggest(r: &SuggestResponse) -> String {
    let mut out = format!(
        "{} suggestion(s) for {:?} (about {} matching titles)\n",
        r.suggestions.len(),
        r.prefix,
        r.total_estimate
    );
    for (i, s) in r.suggestions.iter().enumerate() {
        let _ = writeln!(
            out,
            "{}. {} — archive: {} path: {}",
            i + 1,
            s.title,
            s.archive,
            s.path
        );
    }
    warnings(&mut out, &r.warnings);
    out
}

pub fn context(r: &ContextResponse) -> String {
    let mut out = String::new();
    if r.excerpts.is_empty() {
        let _ = writeln!(
            out,
            "No excerpts found for {:?} in {} archive(s).",
            r.query, r.archives_searched
        );
    } else {
        out.push_str(&zimz_search::render_markdown(r));
        let _ = write!(
            out,
            "\n\n[{} excerpt(s), {} of {} chars, from {} archive(s){}]\n",
            r.excerpts.len(),
            r.chars_used,
            r.budget_chars,
            r.archives_searched,
            if r.fallback_used {
                "; some excerpts match only part of the query"
            } else {
                ""
            }
        );
    }
    warnings(&mut out, &r.warnings);
    out
}

pub fn links(r: &LinksResponse) -> String {
    let mut out = format!(
        "{} internal link(s) in {} (archive: {}, path: {})\n",
        r.total, r.title, r.archive, r.path
    );
    for l in &r.links {
        let _ = writeln!(
            out,
            "- {} -> {}{}",
            l.text,
            l.path,
            match (&l.title, l.exists) {
                (Some(t), true) if t != &l.text => format!(" ({t})"),
                (_, false) => " (missing)".to_string(),
                _ => String::new(),
            }
        );
    }
    if let Some(n) = r.next_offset {
        let _ = writeln!(out, "more: offset={n}");
    }
    out
}

pub fn health(r: &HealthResponse) -> String {
    let mut out = format!(
        "{} archive(s); scan {:.0} ms; extract cache {} entries / {} of {} ({} hits, {} misses)\n",
        r.archives.len(),
        r.scan_ms,
        r.extract_cache.entries,
        human(r.extract_cache.bytes as u64),
        human(r.extract_cache.budget_bytes as u64),
        r.extract_cache.hits,
        r.extract_cache.misses
    );
    for a in &r.archives {
        let _ = writeln!(
            out,
            "- {}: {} articles; fulltext {}{}; title index {}{}; listing {}; checksum {}; cluster cache {} hits / {} misses{}",
            a.name,
            a.article_count,
            if a.has_fulltext_index { "yes" } else { "no" },
            a.fulltext_coverage
                .map(|c| format!(" (coverage {:.0}%)", c * 100.0))
                .unwrap_or_default(),
            if a.has_title_index { "yes" } else { "no" },
            a.title_docs
                .map(|d| format!(" ({d} docs)"))
                .unwrap_or_default(),
            if a.has_title_listing { "yes" } else { "no" },
            a.checksum,
            a.cluster_cache.hits,
            a.cluster_cache.misses,
            if a.problems.is_empty() {
                String::new()
            } else {
                format!("; PROBLEMS: {}", a.problems.join("; "))
            }
        );
    }
    for f in &r.failures {
        let _ = writeln!(out, "failed to open {}: {}", f.file, f.error);
    }
    out
}
