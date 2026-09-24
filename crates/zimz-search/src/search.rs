// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Federated search: fan out to every selected archive, fuse the rankings, fall back
//! to OR when AND is too strict, then compute snippets for the requested page only.

use std::hash::{Hash, Hasher};
use std::time::Instant;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use zimz_glass::search::{Op, Query};
use zimz_glass::{Analyzer, suggest};

use crate::catalog::Mode;
use crate::fusion::{Fused, fuse};
use crate::library::{Library, Slot};
use crate::{Error, Result};

/// Which tier to use per archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ModeSelect {
    /// Full-text index when present, else the title index, else the title listing.
    #[default]
    Auto,
    Fulltext,
    Title,
    Listing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SearchRequest {
    pub query: String,
    /// Archive names, uuids, file stems or globs; empty = every archive.
    #[serde(default)]
    pub archives: Vec<String>,
    #[serde(default)]
    pub mode: ModeSelect,
    /// Hits per page (1..=50).
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Opaque cursor from a previous response to fetch the next page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Maximum characters per snippet; 0 disables snippets.
    #[serde(default = "default_snippet_chars")]
    pub snippet_chars: usize,
    /// Drop hits whose normalised score (1.0 = best) is below this.
    #[serde(default)]
    pub min_score: Option<f64>,
    /// When an AND query fills less than a page, add OR matches after the AND hits.
    #[serde(default = "default_true")]
    pub or_fallback: bool,
}

fn default_limit() -> usize {
    10
}
fn default_snippet_chars() -> usize {
    300
}
fn default_true() -> bool {
    true
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            archives: Vec::new(),
            mode: ModeSelect::Auto,
            limit: default_limit(),
            cursor: None,
            snippet_chars: default_snippet_chars(),
            min_score: None,
            or_fallback: true,
        }
    }
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SearchHit {
    /// 1-based position across the whole result list.
    pub rank: usize,
    pub archive: String,
    /// Path to pass to `read_article` (canonical, redirects resolved when known).
    pub path: String,
    pub title: String,
    /// `zim://archive/path`.
    pub uri: String,
    /// Fused score normalised so the best hit is 1.0.
    pub score: f64,
    /// Xapian's relevance percentage within the archive (100 = best in that archive).
    pub percent: u8,
    pub mode: Mode,
    /// Best window of the article text around the query terms (`**term**` marks
    /// matches), when requested and extractable.
    pub snippet: Option<String>,
    pub word_count: Option<u32>,
    /// True when the hit matched only some of the query terms (OR fallback).
    pub partial: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SearchResponse {
    pub query: String,
    pub hits: Vec<SearchHit>,
    /// Sum of per-archive match counts (before fusion and de-duplication).
    pub total_estimate: u64,
    /// Pass back as `cursor` to get the next page; absent on the last page.
    pub next_cursor: Option<String>,
    /// OR matches were appended because AND matched fewer than a page.
    pub fallback_used: bool,
    /// Number of archives that were searched (see `list_archives` for names).
    pub archives_searched: usize,
    /// Archives that were selected but skipped (no usable index) or failed.
    pub warnings: Vec<String>,
    pub elapsed_ms: u64,
}

/// One per-archive result before fusion.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub path: String,
    pub title: String,
    pub percent: u8,
    pub word_count: Option<u32>,
    /// 1.0 exact title match, 0.5 all query stems in the title, else 0.
    pub title_boost: f64,
    /// Raw BM25 weight (full-text mode) used to compare hits across archives.
    pub weight: f64,
    /// Match strength relative to the best hit of the whole federation, 0..=1.
    pub strength: f64,
}

#[derive(Debug)]
pub(crate) struct ArchiveRun {
    pub slot: usize,
    pub mode: Mode,
    pub total: u64,
    pub candidates: Vec<Candidate>,
}

/// Collapse whitespace, lowercase, strip accents: the key used for exact title matches.
pub(crate) fn normalise(text: &str) -> String {
    Analyzer::remove_accents(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn title_boost(
    analyzer: &Analyzer,
    query_norm: &str,
    query_stems: &[String],
    title: &str,
) -> f64 {
    if query_norm.is_empty() {
        return 0.0;
    }
    if normalise(title) == query_norm {
        return 1.0;
    }
    if query_stems.is_empty() {
        return 0.0;
    }
    let title_stems = analyzer.terms(title);
    if query_stems.iter().all(|q| title_stems.contains(q)) {
        0.5
    } else {
        0.0
    }
}

#[allow(clippy::cast_sign_loss)]
fn percent_of(weight: f64, best: f64) -> u8 {
    if best <= 0.0 || weight <= 0.0 {
        return 0;
    }
    ((weight / best) * 100.0).clamp(1.0, 100.0) as u8
}

/// Run one archive with one operator. Errors are turned into a warning by the caller.
pub(crate) fn run_archive(
    slot: &Slot,
    mode: Mode,
    query: &str,
    op: Op,
    depth: usize,
) -> Result<ArchiveRun> {
    let analyzer = slot.analyzer();
    let q_norm = normalise(query);
    let q_stems = analyzer.terms(query);
    let boost = |title: &str| title_boost(analyzer, &q_norm, &q_stems, title);
    let (total, candidates) = match mode {
        Mode::Fulltext => slot
            .with_fulltext(|db, ix| {
                let q = Query::parse(analyzer, query, op);
                if q.is_empty() {
                    return Ok((0, Vec::new()));
                }
                let res = zimz_glass::search::search(db, &q, 0, depth)?;
                let mut out = Vec::with_capacity(res.hits.len());
                for h in &res.hits {
                    let data = db.docdata_string(h.docid)?.unwrap_or_default();
                    let path = slot.user_path_from_index(&data);
                    let title = db
                        .value_string(h.docid, ix.title_slot)?
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| path.clone());
                    let word_count = ix
                        .wordcount_slot
                        .and_then(|s| db.value_string(h.docid, s).ok().flatten())
                        .and_then(|v| v.trim().parse().ok());
                    out.push(Candidate {
                        title_boost: boost(&title),
                        path,
                        title,
                        percent: h.percent,
                        word_count,
                        weight: h.weight,
                        strength: 0.0,
                    });
                }
                Ok((u64::from(res.total), out))
            })?
            .unwrap_or((0, Vec::new())),
        Mode::Title => slot
            .with_title_index(|db, _ix| {
                let res = suggest::suggest(db, analyzer, query, 0, depth)?;
                let best = res.hits.first().map_or(0.0, |h| h.weight);
                let out = res
                    .hits
                    .iter()
                    .map(|s| {
                        let path =
                            slot.user_path_from_index(s.target_path.as_deref().unwrap_or(&s.path));
                        let title = slot.entry_title(&path).unwrap_or_else(|| s.title.clone());
                        Candidate {
                            title_boost: boost(&title),
                            path,
                            title,
                            percent: percent_of(s.weight, best),
                            word_count: None,
                            weight: 0.0,
                            // title-index hits: half strength, scaled by rank within
                            // the archive (their BM25 is not comparable to full text)
                            strength: 0.5 * f64::from(percent_of(s.weight, best)) / 100.0,
                        }
                    })
                    .collect();
                Ok((u64::from(res.total), out))
            })?
            .unwrap_or((0, Vec::new())),
        Mode::Listing => listing_scan(slot, query, depth, &boost)?,
    };
    Ok(ArchiveRun {
        slot: 0,
        mode,
        total,
        candidates,
    })
}

/// Prefix scan of the title listing, trying the query as typed and capitalised.
fn listing_scan(
    slot: &Slot,
    query: &str,
    depth: usize,
    boost: &dyn Fn(&str) -> f64,
) -> Result<(u64, Vec<Candidate>)> {
    let archive = slot.archive();
    let ns = archive.content_namespace();
    let query = query.trim();
    if query.is_empty() {
        return Ok((0, Vec::new()));
    }
    let mut variants = vec![query.to_string()];
    let mut chars = query.chars();
    if let Some(first) = chars.next() {
        let cap: String = first.to_uppercase().chain(chars).collect();
        if cap != query {
            variants.push(cap);
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut total = 0u64;
    for v in &variants {
        let range = archive.find_title_prefix(ns, v)?;
        total += u64::from(range.end.saturating_sub(range.start));
        for pos in range {
            if out.len() >= depth {
                break;
            }
            let d = archive.entry_by_title_position(pos)?;
            let target = archive.resolve(&d)?;
            if !target.is_item() || !seen.insert(target.index) {
                continue;
            }
            let title = d.title().to_string();
            out.push(Candidate {
                title_boost: boost(&title),
                path: archive.user_path(&target),
                title,
                percent: 100,
                word_count: None,
                weight: 0.0,
                strength: 0.25,
            });
        }
    }
    Ok((total, out))
}

fn cursor_hash(req: &SearchRequest) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    req.query.trim().hash(&mut h);
    let mut archives = req.archives.clone();
    archives.sort();
    archives.hash(&mut h);
    (req.mode as u8).hash(&mut h);
    req.or_fallback.hash(&mut h);
    req.min_score.map(f64::to_bits).hash(&mut h);
    h.finish()
}

pub(crate) fn encode_cursor(hash: u64, offset: usize) -> String {
    format!("{hash:016x}:{offset}")
}

pub(crate) fn decode_cursor(cursor: &str, hash: u64) -> Result<usize> {
    let (h, off) = cursor.trim().split_once(':').ok_or(Error::BadCursor)?;
    let h = u64::from_str_radix(h, 16).map_err(|_| Error::BadCursor)?;
    if h != hash {
        return Err(Error::BadCursor);
    }
    off.parse().map_err(|_| Error::BadCursor)
}

struct CandRef {
    run: usize,
    idx: usize,
}

/// Fused, page-sliced hits before snippets.
struct Ranked {
    run: usize,
    idx: usize,
    score: f64,
    partial: bool,
}

/// Weight of the match-strength term in the fused score, in rank-1 units: a hit with
/// the strongest BM25 weight of the whole federation gets three quarters of a rank-1
/// vote on top of its rank, so a rank-1 hit from an archive that matched weakly does
/// not tie with a rank-1 hit that matched strongly.
const STRENGTH_WEIGHT: f64 = 0.75;

/// Fill `strength` from the raw BM25 weights: relative to the largest weight across
/// all full-text runs. Title/listing candidates keep the constants set when built.
fn assign_strength(runs: &mut [ArchiveRun]) {
    let max = runs
        .iter()
        .filter(|r| r.mode == Mode::Fulltext)
        .flat_map(|r| r.candidates.iter().map(|c| c.weight))
        .fold(0.0_f64, f64::max);
    if max <= 0.0 {
        return;
    }
    for r in runs.iter_mut().filter(|r| r.mode == Mode::Fulltext) {
        for c in &mut r.candidates {
            c.strength = (c.weight / max).clamp(0.0, 1.0);
        }
    }
}

fn fuse_runs(runs: &[ArchiveRun], library: &Library) -> Vec<Fused<CandRef>> {
    let lists: Vec<(f64, Vec<CandRef>)> = runs
        .iter()
        .enumerate()
        .map(|(run, r)| {
            let weight = library.slots()[r.slot].info.priority;
            let items = (0..r.candidates.len())
                .map(|idx| CandRef { run, idx })
                .collect();
            (weight, items)
        })
        .collect();
    fuse(lists, |c| {
        let cand = &runs[c.run].candidates[c.idx];
        cand.title_boost + STRENGTH_WEIGHT * cand.strength
    })
}

impl Library {
    /// Federated search over the selected archives.
    #[allow(clippy::too_many_lines)]
    pub fn search(&self, req: &SearchRequest) -> Result<SearchResponse> {
        let t0 = Instant::now();
        let query = req.query.trim();
        if query.is_empty() {
            return Err(Error::Invalid("query is empty".into()));
        }
        let limit = req.limit.clamp(1, 50);
        let hash = cursor_hash(req);
        let offset = match &req.cursor {
            Some(c) if !c.trim().is_empty() => decode_cursor(c, hash)?,
            _ => 0,
        };
        let selection = self.select(&req.archives)?;
        let depth = (offset + limit).clamp(10, 500);
        let mut warnings = Vec::new();

        // Which archives can serve the requested mode.
        let mut plan: Vec<(usize, Mode)> = Vec::new();
        for &i in &selection {
            match self.slots()[i].mode_for(req.mode) {
                Some(m) => plan.push((i, m)),
                None => warnings.push(format!(
                    "{}: no {} index, skipped",
                    self.slots()[i].name(),
                    match req.mode {
                        ModeSelect::Auto => "search",
                        ModeSelect::Fulltext => "fulltext",
                        ModeSelect::Title => "title",
                        ModeSelect::Listing => "listing",
                    }
                )),
            }
        }

        let run_all = |op: Op, only_fulltext: bool| -> (Vec<ArchiveRun>, Vec<String>) {
            let results: Vec<(usize, Mode, Result<ArchiveRun>)> = plan
                .par_iter()
                .filter(|(_, m)| !only_fulltext || *m == Mode::Fulltext)
                .map(|&(i, m)| (i, m, run_archive(&self.slots()[i], m, query, op, depth)))
                .collect();
            let mut runs = Vec::new();
            let mut warns = Vec::new();
            for (i, _m, r) in results {
                match r {
                    Ok(mut run) => {
                        run.slot = i;
                        runs.push(run);
                    }
                    Err(e) => warns.push(format!("{}: {e}", self.slots()[i].name())),
                }
            }
            (runs, warns)
        };

        let (mut and_runs, w) = run_all(Op::And, false);
        warnings.extend(w);
        assign_strength(&mut and_runs);
        let archives_searched = and_runs.len();
        let fused_and = fuse_runs(&and_runs, self);
        let mut total_estimate: u64 = and_runs.iter().map(|r| r.total).sum();

        // OR fallback: only meaningful for multi-word queries on full-text archives.
        let multi_word = query.split_whitespace().count() >= 2;
        let mut or_runs: Vec<ArchiveRun> = Vec::new();
        let mut fused_or: Vec<Fused<CandRef>> = Vec::new();
        if req.or_fallback && multi_word && fused_and.len() < offset + limit {
            let (runs, w) = run_all(Op::Or, true);
            warnings.extend(w);
            let seen: std::collections::HashSet<(usize, &str)> = and_runs
                .iter()
                .flat_map(|r| r.candidates.iter().map(move |c| (r.slot, c.path.as_str())))
                .collect();
            let mut filtered = Vec::new();
            for mut run in runs {
                let and_total = and_runs
                    .iter()
                    .find(|r| r.slot == run.slot)
                    .map_or(0, |r| r.total);
                total_estimate += run.total.saturating_sub(and_total);
                run.candidates
                    .retain(|c| !seen.contains(&(run.slot, c.path.as_str())));
                filtered.push(run);
            }
            assign_strength(&mut filtered);
            or_runs = filtered;
            fused_or = fuse_runs(&or_runs, self);
        }
        let fallback_used = !fused_or.is_empty();

        // Normalise scores: AND hits relative to the best AND hit; OR-only hits below
        // them (relative to their own best, halved) unless there were no AND hits.
        let and_max = fused_and.first().map_or(0.0, |f| f.score);
        let or_max = fused_or.first().map_or(0.0, |f| f.score);
        let mut ranked: Vec<Ranked> = Vec::with_capacity(fused_and.len() + fused_or.len());
        for f in &fused_and {
            ranked.push(Ranked {
                run: f.item.run,
                idx: f.item.idx,
                score: if and_max > 0.0 {
                    f.score / and_max
                } else {
                    0.0
                },
                partial: false,
            });
        }
        let or_scale = if and_max > 0.0 { 0.5 } else { 1.0 };
        for f in &fused_or {
            ranked.push(Ranked {
                run: f.item.run,
                idx: f.item.idx,
                score: if or_max > 0.0 {
                    or_scale * f.score / or_max
                } else {
                    0.0
                },
                partial: true,
            });
        }
        if let Some(min) = req.min_score {
            ranked.retain(|r| r.score >= min);
        }

        let page_end = (offset + limit).min(ranked.len());
        let next_cursor = (ranked.len() > page_end).then(|| encode_cursor(hash, page_end));
        let page: Vec<&Ranked> = ranked
            .get(offset..page_end)
            .map(|s| s.iter().collect())
            .unwrap_or_default();

        let hits: Vec<SearchHit> = page
            .par_iter()
            .enumerate()
            .map(|(i, r)| {
                let runs = if r.partial { &or_runs } else { &and_runs };
                let run = &runs[r.run];
                let cand = &run.candidates[r.idx];
                let slot = &self.slots()[run.slot];
                let (snippet, word_count) = if req.snippet_chars > 0 {
                    self.snippet(run.slot, &cand.path, query, req.snippet_chars)
                } else {
                    (None, None)
                };
                // The index stores a normalised (often lowercased) title; show the
                // real one for the hits on this page.
                let title = slot
                    .entry_title(&cand.path)
                    .unwrap_or_else(|| cand.title.clone());
                SearchHit {
                    rank: offset + i + 1,
                    archive: slot.name().to_string(),
                    uri: slot.uri(&cand.path),
                    path: cand.path.clone(),
                    title,
                    score: (r.score * 10_000.0).round() / 10_000.0,
                    percent: cand.percent,
                    mode: run.mode,
                    snippet,
                    word_count: cand.word_count.or(word_count),
                    partial: r.partial,
                }
            })
            .collect();

        Ok(SearchResponse {
            query: query.to_string(),
            hits,
            total_estimate,
            next_cursor,
            fallback_used,
            archives_searched,
            warnings,
            elapsed_ms: t0.elapsed().as_millis() as u64,
        })
    }

    /// Best text window for `query` in the article at `path`, and the article's word
    /// count. Extraction failures yield `(None, None)`: a hit without a snippet is
    /// still a hit.
    pub(crate) fn snippet(
        &self,
        slot_idx: usize,
        path: &str,
        query: &str,
        max_chars: usize,
    ) -> (Option<String>, Option<u32>) {
        let slot = &self.slots()[slot_idx];
        let Ok(resolved) = slot.resolve_entry(path) else {
            return (None, None);
        };
        let Ok(doc) = self.document_for_snippet(slot_idx, &resolved.dirent) else {
            return (None, None);
        };
        let analyzer = slot.analyzer();
        let stems = analyzer.terms(query);
        let matcher = |word: &str| {
            let s = analyzer.terms(word);
            s.first().and_then(|w| stems.iter().position(|t| t == w))
        };
        let snip = zimz_extract::best_snippet(&doc.text, matcher, max_chars, Some(("**", "**")));
        let text = snip.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let word_count = u32::try_from(doc.word_count).ok();
        ((!text.is_empty()).then_some(text), word_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trip_and_validation() {
        let req = SearchRequest::new("carbon dioxide");
        let h = cursor_hash(&req);
        let c = encode_cursor(h, 20);
        assert_eq!(decode_cursor(&c, h).unwrap(), 20);
        let other = cursor_hash(&SearchRequest::new("methane"));
        assert!(matches!(decode_cursor(&c, other), Err(Error::BadCursor)));
        assert!(matches!(decode_cursor("garbage", h), Err(Error::BadCursor)));
        assert!(matches!(decode_cursor("zz:1", h), Err(Error::BadCursor)));
    }

    #[test]
    fn cursor_ignores_archive_order_but_not_mode() {
        let mut a = SearchRequest::new("x");
        a.archives = vec!["b".into(), "a".into()];
        let mut b = SearchRequest::new("x");
        b.archives = vec!["a".into(), "b".into()];
        assert_eq!(cursor_hash(&a), cursor_hash(&b));
        b.mode = ModeSelect::Title;
        assert_ne!(cursor_hash(&a), cursor_hash(&b));
    }

    #[test]
    fn title_boost_levels() {
        let an = Analyzer::new(Some("eng"));
        let q = normalise("Carbon dioxide");
        let stems = an.terms("Carbon dioxide");
        assert!((title_boost(&an, &q, &stems, "Carbon  Dioxide") - 1.0).abs() < 1e-9);
        assert!((title_boost(&an, &q, &stems, "Carbon dioxide removal") - 0.5).abs() < 1e-9);
        assert!((title_boost(&an, &q, &stems, "Methane") - 0.0).abs() < 1e-9);
        assert!((title_boost(&an, "", &[], "Methane") - 0.0).abs() < 1e-9);
    }

    #[test]
    fn percent_scaling() {
        assert_eq!(percent_of(2.0, 2.0), 100);
        assert_eq!(percent_of(1.0, 2.0), 50);
        assert_eq!(percent_of(0.001, 2.0), 1);
        assert_eq!(percent_of(0.0, 2.0), 0);
        assert_eq!(percent_of(1.0, 0.0), 0);
    }
}
