// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

use crate::adapter::Adapter;
use crate::links::LinkTarget;

/// A heading and the range of Markdown it governs (up to the next heading of the same
/// or a higher level).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub level: u8,
    pub title: String,
    /// Unique, decoded URI fragment for this heading.
    pub anchor: String,
    /// Byte range in `Document::markdown`, starting at the heading line.
    pub start: usize,
    pub end: usize,
}

/// Keep source IDs when available, and generate unique fragments for other headings.
pub(crate) fn assign_anchors(sections: &mut [Section]) {
    use std::collections::{HashMap, HashSet};

    let reserved: HashSet<String> = sections
        .iter()
        .filter(|s| !s.anchor.is_empty())
        .map(|s| s.anchor.clone())
        .collect();
    let mut used = HashSet::new();
    let mut suffixes = HashMap::new();
    for section in sections {
        let explicit = !section.anchor.is_empty();
        let base = if explicit {
            std::mem::take(&mut section.anchor)
        } else {
            section.title.replace(' ', "_")
        };
        if (explicit || !reserved.contains(&base)) && used.insert(base.clone()) {
            section.anchor = base;
            continue;
        }
        let suffix = suffixes.entry(base.clone()).or_insert(2);
        loop {
            let candidate = format!("{base}_{suffix}");
            *suffix += 1;
            if !reserved.contains(&candidate) && used.insert(candidate.clone()) {
                section.anchor = candidate;
                break;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub text: String,
    pub target: LinkTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineEntry {
    pub index: usize,
    pub level: u8,
    pub title: String,
    /// Characters of Markdown in the section.
    pub chars: usize,
}

/// An extracted entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub title: String,
    pub markdown: String,
    pub text: String,
    pub sections: Vec<Section>,
    pub links: Vec<Link>,
    pub word_count: usize,
    pub adapter: Adapter,
    /// Entry the content was actually read from (a JSON record for app-style scrapers).
    pub source_path: String,
}

impl Document {
    /// Resolve a decoded citation fragment; also accept older title-based citations.
    pub fn find_anchor(&self, fragment: &str) -> Option<usize> {
        self.sections
            .iter()
            .position(|s| s.anchor == fragment)
            .or_else(|| {
                self.sections
                    .iter()
                    .position(|s| s.title.replace(' ', "_") == fragment)
            })
    }
    pub fn outline(&self) -> Vec<OutlineEntry> {
        self.sections
            .iter()
            .enumerate()
            .map(|(index, s)| OutlineEntry {
                index,
                level: s.level,
                title: s.title.clone(),
                chars: self.markdown[s.start..s.end].chars().count(),
            })
            .collect()
    }

    /// Markdown of one section (heading included).
    pub fn section_markdown(&self, index: usize) -> Option<&str> {
        self.sections
            .get(index)
            .map(|s| &self.markdown[s.start..s.end])
    }

    /// Find a section by index (`"3"`) or by case-insensitive title prefix.
    pub fn find_section(&self, name: &str) -> Option<usize> {
        if let Ok(i) = name.trim().parse::<usize>() {
            return (i < self.sections.len()).then_some(i);
        }
        let needle = name.trim().to_lowercase();
        self.sections
            .iter()
            .position(|s| s.title.to_lowercase() == needle)
            .or_else(|| {
                self.sections
                    .iter()
                    .position(|s| s.title.to_lowercase().starts_with(&needle))
            })
            .or_else(|| {
                self.sections
                    .iter()
                    .position(|s| s.title.to_lowercase().contains(&needle))
            })
    }

    /// Internal links, de-duplicated, in document order.
    pub fn internal_links(&self, new_scheme: bool) -> Vec<(String, String)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for l in &self.links {
            if let Some(p) = l.target.user_path(new_scheme)
                && seen.insert(p.clone())
            {
                out.push((l.text.clone(), p));
            }
        }
        out
    }
}
