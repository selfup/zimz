// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! One-call retrieval: search, pick the most relevant section of each hit, and pack
//! the excerpts under a character budget with citations.

use std::fmt::Write as _;
use std::time::Instant;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use zimz_extract::Document;

use crate::library::Library;
use crate::search::{ModeSelect, SearchRequest};
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ContextRequest {
    pub query: String,
    /// Archive names, uuids, file stems or globs; empty = every archive.
    #[serde(default)]
    pub archives: Vec<String>,
    #[serde(default)]
    pub mode: ModeSelect,
    /// Total characters of excerpt text to return (default 12000).
    #[serde(default = "default_budget")]
    pub budget_chars: usize,
    /// Maximum characters per excerpt (default 1500).
    #[serde(default = "default_per_hit")]
    pub per_hit_chars: usize,
    /// Maximum number of excerpts (default 6).
    #[serde(default = "default_max_hits")]
    pub max_hits: usize,
}

fn default_budget() -> usize {
    12_000
}
fn default_per_hit() -> usize {
    1_500
}
fn default_max_hits() -> usize {
    6
}

impl ContextRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            archives: Vec::new(),
            mode: ModeSelect::Auto,
            budget_chars: default_budget(),
            per_hit_chars: default_per_hit(),
            max_hits: default_max_hits(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Excerpt {
    pub archive: String,
    pub path: String,
    pub title: String,
    /// Heading of the section the excerpt comes from (absent: lead / whole article).
    pub section: Option<String>,
    /// Citation: `zim://archive/path#section`.
    pub uri: String,
    /// Search score of the article (1.0 = best hit).
    pub score: f64,
    /// Markdown excerpt.
    pub text: String,
    pub chars: usize,
    /// Distinct query terms found in the excerpt.
    pub matched_terms: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ContextResponse {
    pub query: String,
    pub excerpts: Vec<Excerpt>,
    pub chars_used: usize,
    pub budget_chars: usize,
    /// Search hits examined (some may have been skipped: no text, over budget).
    pub hits_considered: usize,
    pub fallback_used: bool,
    pub archives_searched: Vec<String>,
    pub warnings: Vec<String>,
    pub elapsed_ms: u64,
    /// All excerpts rendered as one Markdown document with a `Source:` line each.
    pub markdown: String,
}

struct Pick {
    section: Option<String>,
    text: String,
    matched: usize,
}

/// Candidate windows: the lead (before the first heading), each section, or the whole
/// document when it has no headings. The most specific section with the most matched
/// terms wins; ties go to the earliest.
fn best_excerpt(
    doc: &Document,
    matcher: &dyn Fn(&str) -> Option<usize>,
    max_chars: usize,
) -> Option<Pick> {
    let mut candidates: Vec<(Option<String>, u8, &str)> = Vec::new();
    if doc.sections.is_empty() {
        candidates.push((None, 0, doc.markdown.as_str()));
    } else {
        let lead = &doc.markdown[..doc.sections[0].start];
        if !lead.trim().is_empty() {
            candidates.push((None, 0, lead));
        }
        for (i, s) in doc.sections.iter().enumerate() {
            if let Some(md) = doc.section_markdown(i) {
                candidates.push((Some(s.title.clone()), s.level, md));
            }
        }
    }
    let mut best: Option<(usize, u8, usize, Pick)> = None; // (matched, level, order, pick)
    for (order, (section, level, md)) in candidates.into_iter().enumerate() {
        // Skip the heading line itself so the window is body text.
        let body = match section {
            Some(_) => md.split_once('\n').map_or("", |(_, rest)| rest),
            None => md,
        };
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        let snip = zimz_extract::best_snippet_with(body, matcher, max_chars, None, false);
        let text = snip.text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        let better = match &best {
            None => true,
            // Prefer the most specific section, but only among sections that
            // actually match something; otherwise the lead wins.
            Some((m, l, _, _)) => {
                snip.matched_terms > *m || (snip.matched_terms == *m && *m > 0 && level > *l)
            }
        };
        if better {
            best = Some((
                snip.matched_terms,
                level,
                order,
                Pick {
                    section,
                    text,
                    matched: snip.matched_terms,
                },
            ));
        }
    }
    best.map(|(_, _, _, p)| p)
}

/// Cut `text` to `max_chars` characters at a word boundary.
fn trim_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(i, _)| i);
    let cut = &text[..end];
    let cut = cut.rfind(char::is_whitespace).map_or(cut, |i| &cut[..i]);
    format!("{}…", cut.trim_end())
}

fn render(excerpts: &[Excerpt]) -> String {
    let mut out = String::new();
    for (i, e) in excerpts.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n---\n\n");
        }
        let _ = write!(out, "## {} ({})", e.title, e.archive);
        if let Some(s) = &e.section {
            let _ = write!(out, " — {s}");
        }
        let _ = write!(out, "\n\n{}\n\nSource: {}", e.text, e.uri);
    }
    out
}

type Picked = Result<Option<(Pick, String)>, String>;

impl Library {
    /// Extract every hit and choose its best excerpt, in parallel. Each entry is the
    /// pick plus the document title, `Ok(None)` when the article has no text, or a
    /// warning string.
    fn pick_excerpts(&self, hits: &[crate::SearchHit], query: &str, per_hit: usize) -> Vec<Picked> {
        hits.par_iter()
            .map(|hit| {
                let idx = self
                    .slot_index(&hit.archive)
                    .ok_or_else(|| format!("{}: archive vanished", hit.uri))?;
                let slot = &self.slots()[idx];
                let resolved = slot
                    .resolve_entry(&hit.path)
                    .map_err(|e| format!("{}: {e}", hit.uri))?;
                let doc = self
                    .document_for_snippet(idx, &resolved.dirent)
                    .map_err(|e| format!("{}: {e}", hit.uri))?;
                let analyzer = slot.analyzer();
                let stems = analyzer.terms(query);
                let matcher = |word: &str| {
                    let s = analyzer.terms(word);
                    s.first().and_then(|w| stems.iter().position(|t| t == w))
                };
                Ok(best_excerpt(&doc, &matcher, per_hit).map(|p| (p, doc.title.clone())))
            })
            .collect()
    }

    /// Search and pack the best section of each hit under `budget_chars`.
    pub fn context(&self, req: &ContextRequest) -> Result<ContextResponse> {
        let t0 = Instant::now();
        if req.query.trim().is_empty() {
            return Err(Error::Invalid("query is empty".into()));
        }
        let max_hits = req.max_hits.clamp(1, 20);
        let per_hit = req.per_hit_chars.clamp(100, 20_000);
        let budget = req.budget_chars.max(per_hit.min(500));
        let search = self.search(&SearchRequest {
            query: req.query.clone(),
            archives: req.archives.clone(),
            mode: req.mode,
            limit: (max_hits * 2).min(50),
            cursor: None,
            snippet_chars: 0,
            min_score: None,
            or_fallback: true,
        })?;
        let mut warnings = search.warnings.clone();
        let picks = self.pick_excerpts(&search.hits, &req.query, per_hit);
        let mut excerpts: Vec<Excerpt> = Vec::new();
        let mut used = 0usize;
        let mut considered = 0usize;
        for (hit, picked) in search.hits.iter().zip(picks) {
            if excerpts.len() >= max_hits || used >= budget {
                break;
            }
            considered += 1;
            let (pick, doc_title) = match picked {
                Ok(Some(p)) => p,
                Ok(None) => continue,
                Err(w) => {
                    warnings.push(w);
                    continue;
                }
            };
            let remaining = budget - used;
            let text = if pick.text.chars().count() > remaining {
                if remaining < 200 {
                    break;
                }
                trim_chars(&pick.text, remaining)
            } else {
                pick.text
            };
            let chars = text.chars().count();
            used += chars;
            let uri = match &pick.section {
                Some(s) => format!("{}#{}", hit.uri, s.replace(' ', "_")),
                None => hit.uri.clone(),
            };
            excerpts.push(Excerpt {
                archive: hit.archive.clone(),
                path: hit.path.clone(),
                title: if doc_title.is_empty() {
                    hit.title.clone()
                } else {
                    doc_title
                },
                section: pick.section,
                uri,
                score: hit.score,
                text,
                chars,
                matched_terms: pick.matched,
            });
        }
        let markdown = render(&excerpts);
        Ok(ContextResponse {
            query: req.query.trim().to_string(),
            excerpts,
            chars_used: used,
            budget_chars: budget,
            hits_considered: considered,
            fallback_used: search.fallback_used,
            archives_searched: search.archives_searched,
            warnings,
            elapsed_ms: t0.elapsed().as_millis() as u64,
            markdown,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zimz_extract::{Adapter, Section};

    fn doc(md: &str, sections: Vec<(u8, &str)>) -> Document {
        let mut secs = Vec::new();
        for (level, title) in sections {
            let needle = format!("{} {title}\n", "#".repeat(usize::from(level)));
            let start = md.find(&needle).expect("heading present");
            secs.push(Section {
                level,
                title: title.to_string(),
                start,
                end: md.len(),
            });
        }
        for i in 0..secs.len() {
            let level = secs[i].level;
            secs[i].end = secs[i + 1..]
                .iter()
                .find(|s| s.level <= level)
                .map_or(md.len(), |s| s.start);
        }
        Document {
            title: "T".into(),
            markdown: md.to_string(),
            text: md.to_string(),
            sections: secs,
            links: Vec::new(),
            word_count: md.split_whitespace().count(),
            adapter: Adapter::Generic,
            source_path: "T".into(),
        }
    }

    #[test]
    fn picks_the_section_with_most_matches() {
        let md = "Lead text about nothing.\n\n## One\n\nApples grow on trees.\n\n## Two\n\nPears and apples are fruit; apples again.\n\n### Deep\n\nOnly pears here.\n";
        let d = doc(md, vec![(2, "One"), (2, "Two"), (3, "Deep")]);
        let matcher = |w: &str| {
            ["apples", "pears"]
                .iter()
                .position(|t| w.to_lowercase() == *t)
        };
        let p = best_excerpt(&d, &matcher, 200).unwrap();
        assert_eq!(p.section.as_deref(), Some("Two"));
        assert_eq!(p.matched, 2);
        assert!(p.text.contains("Pears and apples"));
    }

    #[test]
    fn falls_back_to_lead_when_nothing_matches() {
        let md = "Lead text.\n\n## One\n\nBody.\n";
        let d = doc(md, vec![(2, "One")]);
        let p = best_excerpt(&d, &|_| None, 200).unwrap();
        assert_eq!(p.section, None);
        assert_eq!(p.text, "Lead text.");
    }

    #[test]
    fn whole_document_without_headings() {
        let d = doc("Just a paragraph with apples.", vec![]);
        let matcher = |w: &str| (w.to_lowercase() == "apples").then_some(0);
        let p = best_excerpt(&d, &matcher, 200).unwrap();
        assert_eq!(p.matched, 1);
        assert_eq!(p.section, None);
    }

    #[test]
    fn trim_respects_words() {
        assert_eq!(trim_chars("hello world again", 11), "hello…");
        assert_eq!(trim_chars("short", 11), "short");
    }
}
