// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Reading one article: budgeted content windows, outlines and links.

use serde::{Deserialize, Serialize};
use zimz_core::Dirent;

use crate::library::Library;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Format {
    /// Markdown with headings, lists, tables and `zim://` links (default).
    #[default]
    Markdown,
    /// Plain text (no markup, no links).
    Text,
    /// The stored HTML, untouched.
    Html,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArticleRequest {
    pub archive: String,
    /// Article path from a search hit, a `zim://` URI, or a title.
    pub path: String,
    #[serde(default)]
    pub format: Format,
    /// Maximum characters returned (default 8000). The outline is included whenever the
    /// article is longer, so a follow-up call can ask for one `section`.
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    /// Character offset to continue from (use `next_offset` of the previous call).
    #[serde(default)]
    pub offset: usize,
    /// Return only this section: its index from the outline or (a prefix of) its title.
    #[serde(default)]
    pub section: Option<String>,
}

fn default_max_chars() -> usize {
    8000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OutlineItem {
    /// Index to pass as `section`.
    pub index: usize,
    /// Heading level (1 = top).
    pub level: u8,
    pub title: String,
    /// Characters of Markdown in the section, subsections included.
    pub chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArticleResponse {
    pub archive: String,
    /// Canonical path of the article that was read (redirects resolved).
    pub path: String,
    pub title: String,
    pub uri: String,
    pub format: Format,
    pub content: String,
    /// Character offset of `content` within the whole document (or section).
    pub offset: usize,
    /// Offset to continue from; absent when `content` reaches the end.
    pub next_offset: Option<usize>,
    /// Characters in the whole document (or selected section).
    pub total_chars: usize,
    pub truncated: bool,
    pub word_count: Option<usize>,
    /// Section headings, present when the content had to be cut.
    pub outline: Option<Vec<OutlineItem>>,
    pub links_count: Option<usize>,
    /// Title of the section that was selected, when `section` was given.
    pub section: Option<String>,
    pub redirected_from: Option<String>,
    pub adapter: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OutlineResponse {
    pub archive: String,
    pub path: String,
    pub title: String,
    pub uri: String,
    pub word_count: usize,
    pub total_chars: usize,
    pub sections: Vec<OutlineItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LinksRequest {
    pub archive: String,
    pub path: String,
    /// Links per page (default 100, max 500).
    #[serde(default = "default_links_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

fn default_links_limit() -> usize {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LinkInfo {
    /// Anchor text in the article.
    pub text: String,
    /// Path to pass to `read_article`.
    pub path: String,
    /// Title of the target article, when it exists.
    pub title: Option<String>,
    pub uri: String,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LinksResponse {
    pub archive: String,
    pub path: String,
    pub title: String,
    pub links: Vec<LinkInfo>,
    /// Distinct internal links in the article.
    pub total: usize,
    pub next_offset: Option<usize>,
}

/// `(window, total_chars, next_offset)` for a character range of `s`.
pub(crate) fn char_window(
    s: &str,
    offset: usize,
    max_chars: usize,
) -> (String, usize, Option<usize>) {
    let total = s.chars().count();
    if offset >= total {
        return (String::new(), total, None);
    }
    let start = s.char_indices().nth(offset).map_or(s.len(), |(i, _)| i);
    let end_chars = offset.saturating_add(max_chars).min(total);
    let end = if end_chars >= total {
        s.len()
    } else {
        s.char_indices().nth(end_chars).map_or(s.len(), |(i, _)| i)
    };
    let next = (end_chars < total).then_some(end_chars);
    (s[start..end].to_string(), total, next)
}

pub(crate) fn outline_items(doc: &zimz_extract::Document) -> Vec<OutlineItem> {
    doc.outline()
        .into_iter()
        .map(|o| OutlineItem {
            index: o.index,
            level: o.level,
            title: o.title,
            chars: o.chars,
        })
        .collect()
}

impl Library {
    pub fn read_article(&self, req: &ArticleRequest) -> Result<ArticleResponse> {
        let idx = self
            .slot_index(&req.archive)
            .ok_or_else(|| Error::NoSuchArchive(req.archive.clone()))?;
        let slot = &self.slots()[idx];
        let resolved = slot.resolve_entry(&req.path)?;
        let max_chars = req.max_chars.max(1);
        let uri = slot.uri(&resolved.path);

        if req.format == Format::Html {
            let data = slot.archive().item_data(&resolved.dirent)?;
            let html = String::from_utf8_lossy(&data);
            let (content, total, next) = char_window(&html, req.offset, max_chars);
            return Ok(ArticleResponse {
                archive: slot.name().to_string(),
                path: resolved.path,
                title: resolved.dirent.title().to_string(),
                uri,
                format: Format::Html,
                content,
                offset: req.offset,
                next_offset: next,
                total_chars: total,
                truncated: next.is_some(),
                word_count: None,
                outline: None,
                links_count: None,
                section: None,
                redirected_from: resolved.redirected_from,
                adapter: format!("{:?}", slot.adapter()),
            });
        }

        let doc = self.document(idx, &resolved.dirent)?;
        let (body, section_title): (&str, Option<String>) = match &req.section {
            Some(name) => {
                let i = doc.find_section(name).ok_or_else(|| {
                    let known: Vec<String> = doc
                        .sections
                        .iter()
                        .enumerate()
                        .take(60)
                        .map(|(i, s)| format!("{i}={:?}", s.title))
                        .collect();
                    Error::Invalid(format!(
                        "no section matching {name:?}; sections: {}",
                        known.join(", ")
                    ))
                })?;
                (
                    doc.section_markdown(i).unwrap_or(""),
                    Some(doc.sections[i].title.clone()),
                )
            }
            None => match req.format {
                Format::Text => (doc.text.as_str(), None),
                _ => (doc.markdown.as_str(), None),
            },
        };
        let (content, total, next) = char_window(body, req.offset, max_chars);
        let truncated = next.is_some();
        Ok(ArticleResponse {
            archive: slot.name().to_string(),
            path: resolved.path,
            title: if doc.title.is_empty() {
                resolved.dirent.title().to_string()
            } else {
                doc.title.clone()
            },
            uri,
            format: if req.section.is_some() {
                Format::Markdown
            } else {
                req.format
            },
            content,
            offset: req.offset,
            next_offset: next,
            total_chars: total,
            truncated,
            word_count: Some(doc.word_count),
            outline: (truncated && req.section.is_none()).then(|| outline_items(&doc)),
            links_count: Some(
                doc.internal_links(slot.archive().uses_new_namespace_scheme())
                    .len(),
            ),
            section: section_title,
            redirected_from: resolved.redirected_from,
            adapter: format!("{:?}", doc.adapter),
        })
    }

    pub fn outline(&self, archive: &str, path: &str) -> Result<OutlineResponse> {
        let idx = self
            .slot_index(archive)
            .ok_or_else(|| Error::NoSuchArchive(archive.to_string()))?;
        let slot = &self.slots()[idx];
        let resolved = slot.resolve_entry(path)?;
        let doc = self.document(idx, &resolved.dirent)?;
        Ok(OutlineResponse {
            archive: slot.name().to_string(),
            uri: slot.uri(&resolved.path),
            path: resolved.path,
            title: doc.title.clone(),
            word_count: doc.word_count,
            total_chars: doc.markdown.chars().count(),
            sections: outline_items(&doc),
        })
    }

    pub fn links(&self, req: &LinksRequest) -> Result<LinksResponse> {
        let idx = self
            .slot_index(&req.archive)
            .ok_or_else(|| Error::NoSuchArchive(req.archive.clone()))?;
        let slot = &self.slots()[idx];
        let resolved = slot.resolve_entry(&req.path)?;
        let doc = self.document(idx, &resolved.dirent)?;
        let archive = slot.archive();
        let all = doc.internal_links(archive.uses_new_namespace_scheme());
        let limit = req.limit.clamp(1, 500);
        let end = req.offset.saturating_add(limit).min(all.len());
        let links = all
            .get(req.offset..end)
            .unwrap_or(&[])
            .iter()
            .map(|(text, path)| {
                let target: Option<std::sync::Arc<Dirent>> = archive
                    .entry_by_path_compat(path)
                    .ok()
                    .flatten()
                    .and_then(|d| archive.resolve(&d).ok())
                    .filter(|d| d.is_item());
                let canonical = target
                    .as_ref()
                    .map_or_else(|| path.clone(), |d| archive.user_path(d));
                LinkInfo {
                    text: text.clone(),
                    uri: slot.uri(&canonical),
                    title: target.as_ref().map(|d| d.title().to_string()),
                    exists: target.is_some(),
                    path: canonical,
                }
            })
            .collect();
        Ok(LinksResponse {
            archive: slot.name().to_string(),
            path: resolved.path,
            title: doc.title.clone(),
            links,
            total: all.len(),
            next_offset: (end < all.len()).then_some(end),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_counts_chars_not_bytes() {
        let s = "héllo wörld";
        let (w, total, next) = char_window(s, 0, 5);
        assert_eq!(w, "héllo");
        assert_eq!(total, 11);
        assert_eq!(next, Some(5));
        let (w, _, next) = char_window(s, 5, 100);
        assert_eq!(w, " wörld");
        assert_eq!(next, None);
        let (w, _, next) = char_window(s, 50, 5);
        assert_eq!(w, "");
        assert_eq!(next, None);
    }
}
