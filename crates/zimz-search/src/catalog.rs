// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Per-archive catalogue entries built from ZIM metadata.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use zimz_core::metadata::parse_tags;
use zimz_core::{Archive, TitleIndex};

use crate::Result;

/// How an archive is searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Mode {
    /// Embedded Xapian full-text index (`X/fulltext/xapian`): BM25 over article bodies.
    Fulltext,
    /// Embedded Xapian title index (`X/title/xapian`): word-prefix matching on titles.
    Title,
    /// Title listing prefix scan (no Xapian index in the archive).
    Listing,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Fulltext => "fulltext",
            Mode::Title => "title",
            Mode::Listing => "listing",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What an agent needs to know to pick an archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArchiveInfo {
    /// Unique short name that addresses the archive in every other call (the `Name`
    /// metadata, or the file name when that is missing or duplicated).
    pub name: String,
    /// Path of the `.zim` file (first part for split archives).
    pub file: String,
    pub uuid: String,
    pub title: Option<String>,
    pub description: Option<String>,
    /// ISO 639-3 code(s), e.g. `eng` or `fra,eng`.
    pub language: Option<String>,
    pub creator: Option<String>,
    pub publisher: Option<String>,
    /// `YYYY-MM-DD` creation date.
    pub date: Option<String>,
    pub flavour: Option<String>,
    pub scraper: Option<String>,
    /// Plain tags (system `_key:value` tags are folded into the other fields).
    pub tags: Vec<String>,
    /// `_category:` tag, e.g. `wikipedia`, `devdocs`, `gutenberg`.
    pub category: Option<String>,
    pub size_bytes: u64,
    pub entry_count: u32,
    /// Front articles (what search results can point to).
    pub article_count: u32,
    /// Images, video and audio entries (from the `Counter` metadata).
    pub media_count: u64,
    /// HTML items (from the `Counter` metadata): the pages a full-text index can
    /// cover. `article_count` also counts redirects.
    pub html_count: Option<u64>,
    pub has_fulltext_index: bool,
    pub has_title_index: bool,
    /// Documents in the full-text index, when there is one.
    pub fulltext_docs: Option<u32>,
    /// Documents in the title index, when there is one.
    pub title_docs: Option<u32>,
    /// How `search` will query this archive (`None`: no index and no title listing).
    pub search_mode: Option<Mode>,
    /// Ranking weight from the library configuration (1.0 = neutral).
    pub priority: f64,
    /// Path of the main page, when the archive declares one.
    pub main_page: Option<String>,
    /// `major.minor` of the ZIM header.
    pub zim_version: String,
    /// Content adapter chosen for Markdown extraction (e.g. `MwOffliner`, `DevDocs`).
    pub adapter: String,
}

fn meta(archive: &Archive, key: &str) -> Option<String> {
    archive
        .metadata_string(key)
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn html_count(counter: &[(String, u64)]) -> Option<u64> {
    let n: u64 = counter
        .iter()
        .filter(|(m, _)| m.starts_with("text/html"))
        .map(|(_, n)| *n)
        .sum();
    (n > 0).then_some(n)
}

fn media_count(counter: &[(String, u64)]) -> u64 {
    counter
        .iter()
        .filter(|(m, _)| {
            m.starts_with("image/") || m.starts_with("video/") || m.starts_with("audio/")
        })
        .map(|(_, n)| *n)
        .sum()
}

impl ArchiveInfo {
    /// Read everything from the archive's metadata. `name`, `priority`, index document
    /// counts and `search_mode` are filled in by the library afterwards.
    pub fn read(archive: &Archive, file: &Path, adapter: &str) -> Result<Self> {
        let header = archive.header();
        let (tags, system) = meta(archive, "Tags")
            .map(|t| parse_tags(&t))
            .unwrap_or_default();
        let category = system
            .iter()
            .find(|(k, _)| k == "category")
            .map(|(_, v)| v.clone());
        let counter = archive.counter().unwrap_or_default();
        let main_page = archive.main_entry()?.map(|d| archive.user_path(&d));
        let has_listing = !matches!(archive.title_index(), TitleIndex::None);
        let has_fulltext_index = archive.has_fulltext_index();
        let has_title_index = archive.has_title_xapian_index();
        let search_mode = if has_fulltext_index {
            Some(Mode::Fulltext)
        } else if has_title_index {
            Some(Mode::Title)
        } else if has_listing {
            Some(Mode::Listing)
        } else {
            None
        };
        Ok(Self {
            name: String::new(),
            file: file.display().to_string(),
            uuid: archive.uuid(),
            title: meta(archive, "Title"),
            description: meta(archive, "Description"),
            language: meta(archive, "Language"),
            creator: meta(archive, "Creator"),
            publisher: meta(archive, "Publisher"),
            date: meta(archive, "Date"),
            flavour: meta(archive, "Flavour"),
            scraper: meta(archive, "Scraper"),
            tags,
            category,
            size_bytes: archive.size(),
            entry_count: header.entry_count,
            article_count: archive.article_count()?,
            media_count: media_count(&counter),
            html_count: html_count(&counter),
            has_fulltext_index,
            has_title_index,
            fulltext_docs: None,
            title_docs: None,
            search_mode,
            priority: 1.0,
            main_page,
            zim_version: format!("{}.{}", header.major, header.minor),
            adapter: adapter.to_string(),
        })
    }

    /// The metadata `Name`, sanitised for use as an identifier, if present.
    pub fn metadata_name(archive: &Archive) -> Option<String> {
        meta(archive, "Name")
            .map(|n| sanitize_name(&n))
            .filter(|n| !n.is_empty())
    }

    /// Case-insensitive substring match over the descriptive fields.
    pub fn matches_filter(&self, filter: &str) -> bool {
        let f = filter.trim().to_lowercase();
        if f.is_empty() {
            return true;
        }
        let fields = [
            Some(&self.name),
            self.title.as_ref(),
            self.description.as_ref(),
            self.language.as_ref(),
            self.category.as_ref(),
            self.creator.as_ref(),
            self.flavour.as_ref(),
        ];
        fields
            .into_iter()
            .flatten()
            .any(|v| v.to_lowercase().contains(&f))
            || self.tags.iter().any(|t| t.to_lowercase().contains(&f))
    }
}

/// Keep names shell- and URI-friendly: whitespace and path separators become `_`.
pub fn sanitize_name(raw: &str) -> String {
    raw.trim()
        .chars()
        .map(|c| {
            if c.is_whitespace() || matches!(c, '/' | '\\' | ':' | '#' | '?') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// `foo_en_all_2026-01.zim` → `foo_en_all_2026-01`; split parts drop the `aa` suffix.
pub fn file_stem_name(file: &Path) -> String {
    let stem = file
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = stem
        .strip_suffix(".zimaa")
        .or_else(|| stem.strip_suffix(".zim"))
        .unwrap_or(&stem);
    sanitize_name(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_strips_extensions() {
        assert_eq!(file_stem_name(Path::new("/x/a_b.zim")), "a_b");
        assert_eq!(file_stem_name(Path::new("/x/a_b.zimaa")), "a_b");
        assert_eq!(file_stem_name(Path::new("weird name.zim")), "weird_name");
    }

    #[test]
    fn media_counts_only_media() {
        let c = vec![
            ("image/png".to_string(), 3),
            ("text/html".to_string(), 10),
            ("text/html; charset=utf-8".to_string(), 2),
            ("video/webm".to_string(), 1),
        ];
        assert_eq!(media_count(&c), 4);
        assert_eq!(html_count(&c), Some(12));
        assert_eq!(html_count(&[]), None);
    }
}
