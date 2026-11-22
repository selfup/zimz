// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! One-call retrieval: search, pick the most relevant section of each hit, and pack
//! the excerpts under a character budget with citations.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::Instant;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use zimz_extract::Document;

use crate::article::section_uri;
use crate::library::Library;
use crate::search::{ModeSelect, SearchRequest, normalise, split_phrases};
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
    /// Number of archives that were searched.
    pub archives_searched: usize,
    pub warnings: Vec<String>,
    pub elapsed_ms: u64,
}

struct Pick {
    section: Option<usize>,
    text: String,
    matched: usize,
}

fn matched_terms(text: &str, matcher: &dyn Fn(&str) -> Option<usize>) -> HashSet<usize> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\''))
        .filter(|word| !word.is_empty())
        .filter_map(matcher)
        .collect()
}

/// Score each heading with its own body so descendant passages cite their own heading.
fn best_excerpt(
    doc: &Document,
    matcher: &dyn Fn(&str) -> Option<usize>,
    max_chars: usize,
    title_lookup: bool,
) -> Option<Pick> {
    let end = doc.sections.first().map_or(doc.markdown.len(), |s| s.start);
    let lead = (None, &doc.markdown[..end]);
    let sections = doc.sections.iter().enumerate().map(|(i, section)| {
        let end = doc
            .sections
            .get(i + 1)
            .map_or(doc.markdown.len(), |s| s.start);
        let md = &doc.markdown[section.start..end];
        (Some(i), md.split_once('\n').map_or("", |(_, body)| body))
    });
    let mut best = None;
    let mut best_score = (0, 0, 0);
    for (index, body) in std::iter::once(lead).chain(sections) {
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        if title_lookup {
            let text = trim_chars(body, max_chars);
            return Some(Pick {
                section: index,
                matched: matched_terms(&text, matcher).len(),
                text,
            });
        }
        let snip = zimz_extract::best_snippet_with(body, matcher, max_chars, None, false);
        let mut terms = matched_terms(&snip.text, matcher);
        let matched = terms.len();
        let mut heading_count = 0;
        let mut depth = 0;
        if let Some(i) = index {
            let section = &doc.sections[i];
            let heading = matched_terms(&section.title, matcher);
            heading_count = heading.len();
            terms.extend(heading);
            if !terms.is_empty() {
                depth = section.level;
            }
        }
        let score = (terms.len(), heading_count, depth);
        if best.is_none() || score > best_score {
            best = Some(Pick {
                section: index,
                text: snip.text,
                matched,
            });
            best_score = score;
        }
    }
    best
}

/// Cut `text` to `max_chars` characters at a word boundary.
fn trim_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .nth(max_chars - 1)
        .map_or(text.len(), |(i, _)| i);
    let cut = &text[..end];
    let cut = cut.rfind(char::is_whitespace).map_or(cut, |i| &cut[..i]);
    format!("{}…", cut.trim_end())
}

/// All excerpts as one Markdown document with a `Source:` line each (the text
/// rendering; it is not part of the structured response to keep payloads small).
pub fn render_markdown(response: &ContextResponse) -> String {
    let mut out = String::new();
    for (i, e) in response.excerpts.iter().enumerate() {
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

type Picked = Result<Option<Excerpt>, String>;

impl Library {
    /// Extract and cite one passage per hit in rank order. Empty documents are skipped.
    fn pick_excerpts(&self, hits: &[crate::SearchHit], query: &str, per_hit: usize) -> Vec<Picked> {
        let (query, _) = split_phrases(query);
        let title_query = normalise(&query);
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
                let stems = analyzer.terms(&query);
                let matcher = |word: &str| {
                    let s = analyzer.terms(word);
                    s.first().and_then(|w| stems.iter().position(|t| t == w))
                };
                let title = if doc.title.is_empty() {
                    &hit.title
                } else {
                    &doc.title
                };
                let title_lookup =
                    normalise(title) == title_query || normalise(&hit.title) == title_query;
                let uri = slot.uri(&resolved.path);
                Ok(
                    best_excerpt(&doc, &matcher, per_hit, title_lookup).map(|p| {
                        let section = p.section.map(|i| &doc.sections[i]);
                        Excerpt {
                            archive: hit.archive.clone(),
                            path: resolved.path,
                            title: title.clone(),
                            uri: section
                                .map_or_else(|| uri.clone(), |s| section_uri(&uri, &s.anchor)),
                            section: section.map(|s| s.title.clone()),
                            score: hit.score,
                            chars: p.text.chars().count(),
                            text: p.text,
                            matched_terms: p.matched,
                        }
                    }),
                )
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
        let budget = req.budget_chars;
        let per_hit = req.per_hit_chars.min(20_000).min(budget);
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
        let picks = if per_hit == 0 {
            Vec::new()
        } else {
            self.pick_excerpts(&search.hits, &req.query, per_hit)
        };
        let mut excerpts: Vec<Excerpt> = Vec::new();
        let mut used = 0usize;
        let mut considered = 0usize;
        for picked in picks {
            if excerpts.len() >= max_hits || used >= budget {
                break;
            }
            considered += 1;
            let mut excerpt = match picked {
                Ok(Some(p)) => p,
                Ok(None) => continue,
                Err(w) => {
                    warnings.push(w);
                    continue;
                }
            };
            let remaining = budget - used;
            if excerpt.chars > remaining {
                if remaining < 200 {
                    break;
                }
                excerpt.text = trim_chars(&excerpt.text, remaining);
                excerpt.chars = excerpt.text.chars().count();
                let analyzer = self.get(&excerpt.archive)?.analyzer();
                let stems = analyzer.terms(&req.query);
                excerpt.matched_terms = matched_terms(&excerpt.text, &|word| {
                    analyzer
                        .terms(word)
                        .first()
                        .and_then(|w| stems.iter().position(|s| s == w))
                })
                .len();
            }
            used += excerpt.chars;
            excerpts.push(excerpt);
        }
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
                anchor: title.replace(' ', "_"),
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
        let p = best_excerpt(&d, &matcher, 200, false).unwrap();
        assert_eq!(p.section, Some(1));
        assert_eq!(p.matched, 2);
        assert!(p.text.contains("Pears and apples"));
    }

    #[test]
    fn falls_back_to_lead_when_nothing_matches() {
        let md = "Lead text.\n\n## One\n\nBody.\n";
        let d = doc(md, vec![(2, "One")]);
        let p = best_excerpt(&d, &|_| None, 200, false).unwrap();
        assert_eq!(p.section, None);
        assert_eq!(p.text, "Lead text.");
    }

    #[test]
    fn whole_document_without_headings() {
        let d = doc("Just a paragraph with apples.", vec![]);
        let matcher = |w: &str| (w.to_lowercase() == "apples").then_some(0);
        let p = best_excerpt(&d, &matcher, 200, false).unwrap();
        assert_eq!(p.matched, 1);
        assert_eq!(p.section, None);
    }

    #[test]
    fn title_lookup_opens_at_the_lead_and_a_heading_can_win_a_focused_query() {
        let d = doc(
            "Gödel proved two theorems.\n\n## Rosser\n\nA stronger result.\n\n### History\n\nRosser discussed Gödel.\n",
            vec![(2, "Rosser"), (3, "History")],
        );
        let title = best_excerpt(&d, &|w| (w == "Gödel").then_some(0), 100, true).unwrap();
        assert_eq!(title.section, None);
        assert!(title.text.starts_with("Gödel proved"));
        let focused = best_excerpt(&d, &|w| (w == "Rosser").then_some(0), 100, false).unwrap();
        assert_eq!(focused.section, Some(0));
        assert_eq!(focused.text, "A stronger result.");
    }

    #[test]
    fn trim_respects_words() {
        assert_eq!(trim_chars("hello world again", 11), "hello…");
        assert_eq!(trim_chars("short", 11), "short");
        assert_eq!(trim_chars("ééééé", 3), "éé…");
    }
}
