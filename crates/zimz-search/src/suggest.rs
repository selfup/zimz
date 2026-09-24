// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Federated title suggestions (type-ahead).

use std::time::Instant;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use zimz_glass::suggest;

use crate::catalog::Mode;
use crate::fusion::fuse;
use crate::library::{Library, Slot};
use crate::search::{normalise, title_boost};
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SuggestRequest {
    /// Beginning of a title, one or more words (the last may be partial).
    pub prefix: String,
    /// Archive names, uuids, file stems or globs; empty = every archive.
    #[serde(default)]
    pub archives: Vec<String>,
    /// Suggestions to return (1..=50).
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SuggestionHit {
    pub archive: String,
    pub path: String,
    pub title: String,
    pub uri: String,
    pub mode: Mode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SuggestResponse {
    pub prefix: String,
    pub suggestions: Vec<SuggestionHit>,
    pub total_estimate: u64,
    /// Number of archives that were searched.
    pub archives_searched: usize,
    pub warnings: Vec<String>,
    pub elapsed_ms: u64,
}

struct Cand {
    path: String,
    title: String,
    boost: f64,
}

type Run = (Mode, u64, Vec<Cand>);
type Tagged = (usize, Mode, Cand);

fn run(slot: &Slot, prefix: &str, limit: usize) -> Result<Run> {
    let analyzer = slot.analyzer();
    let q_norm = normalise(prefix);
    let q_stems = analyzer.terms(prefix);
    if let Some((total, cands)) = slot.with_title_index(|db, _| {
        let res = suggest::suggest(db, analyzer, prefix, 0, limit)?;
        let cands = res
            .hits
            .iter()
            .map(|s| {
                let path = slot.user_path_from_index(s.target_path.as_deref().unwrap_or(&s.path));
                let title = slot.entry_title(&path).unwrap_or_else(|| s.title.clone());
                Cand {
                    boost: title_boost(analyzer, &q_norm, &q_stems, &title),
                    path,
                    title,
                }
            })
            .collect();
        Ok((u64::from(res.total), cands))
    })? {
        return Ok((Mode::Title, total, cands));
    }
    // No title index: prefix scan of the listing (as typed, then capitalised).
    let archive = slot.archive();
    let ns = archive.content_namespace();
    let mut variants = vec![prefix.to_string()];
    let mut chars = prefix.chars();
    if let Some(first) = chars.next() {
        let cap: String = first.to_uppercase().chain(chars).collect();
        if cap != prefix {
            variants.push(cap);
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut cands = Vec::new();
    let mut total = 0u64;
    for v in &variants {
        let range = archive.find_title_prefix(ns, v)?;
        total += u64::from(range.end.saturating_sub(range.start));
        for pos in range {
            if cands.len() >= limit {
                break;
            }
            let d = archive.entry_by_title_position(pos)?;
            let target = archive.resolve(&d)?;
            if !target.is_item() || !seen.insert(target.index) {
                continue;
            }
            let title = d.title().to_string();
            cands.push(Cand {
                path: archive.user_path(&target),
                boost: title_boost(analyzer, &q_norm, &q_stems, &title),
                title,
            });
        }
    }
    Ok((Mode::Listing, total, cands))
}

impl Library {
    pub fn suggest(&self, req: &SuggestRequest) -> Result<SuggestResponse> {
        let t0 = Instant::now();
        let prefix = req.prefix.trim();
        if prefix.is_empty() {
            return Err(Error::Invalid("prefix is empty".into()));
        }
        let limit = req.limit.clamp(1, 50);
        let selection = self.select(&req.archives)?;
        let results: Vec<(usize, Result<Run>)> = selection
            .par_iter()
            .map(|&i| (i, run(&self.slots()[i], prefix, limit)))
            .collect();
        let mut warnings = Vec::new();
        let mut runs = Vec::new();
        for (i, r) in results {
            match r {
                Ok(v) => runs.push((i, v)),
                Err(e) => warnings.push(format!("{}: {e}", self.slots()[i].name())),
            }
        }
        let archives_searched = runs.len();
        let total_estimate = runs.iter().map(|(_, (_, t, _))| *t).sum();
        let lists: Vec<(f64, Vec<Tagged>)> = runs
            .into_iter()
            .map(|(i, (mode, _, cands))| {
                let w = self.slots()[i].info.priority;
                (w, cands.into_iter().map(|c| (i, mode, c)).collect())
            })
            .collect();
        let fused = fuse(lists, |(_, _, c)| c.boost);
        let suggestions = fused
            .into_iter()
            .take(limit)
            .map(|f| {
                let (i, mode, c) = f.item;
                let slot = &self.slots()[i];
                SuggestionHit {
                    archive: slot.name().to_string(),
                    uri: slot.uri(&c.path),
                    path: c.path,
                    title: c.title,
                    mode,
                }
            })
            .collect();
        Ok(SuggestResponse {
            prefix: prefix.to_string(),
            suggestions,
            total_estimate,
            archives_searched,
            warnings,
            elapsed_ms: t0.elapsed().as_millis() as u64,
        })
    }
}
