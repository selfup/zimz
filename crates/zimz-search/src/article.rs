// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Reading one article: budgeted content windows, outlines and links.

use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use zimz_core::Dirent;
use zimz_extract::Document;

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
    /// Archive name; optional when path is a full zim:// citation.
    #[serde(default)]
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
    /// Read these sections in order as Markdown passages sharing max_chars.
    /// Continue a passage by reading its URI with its next_offset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<String>,
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
    pub uri: String,
    /// Characters of Markdown in the section, subsections included.
    pub chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Passage {
    pub section: String,
    pub uri: String,
    pub content: String,
    pub offset: usize,
    pub total_chars: usize,
    pub next_offset: Option<usize>,
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
    /// For a sections request, these passages carry the content and continuation offsets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passages: Vec<Passage>,
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

pub(crate) fn section_uri(uri: &str, anchor: &str) -> String {
    format!("{uri}#{}", zimz_extract::links::uri_component(anchor))
}

fn outline_items(doc: &Document, uri: &str) -> Vec<OutlineItem> {
    doc.outline()
        .into_iter()
        .map(|o| OutlineItem {
            index: o.index,
            level: o.level,
            title: o.title,
            uri: section_uri(uri, &doc.sections[o.index].anchor),
            chars: o.chars,
        })
        .collect()
}

fn missing_section(doc: &Document, name: &str) -> Error {
    let known: Vec<_> = doc
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
}

/// Bare entry paths historically allow unused fragments. A full citation must resolve exactly.
fn selected_section(
    doc: &Document,
    name: Option<&str>,
    fragment: Option<&str>,
    citation: bool,
) -> Result<Option<usize>> {
    if let Some(name) = name {
        return doc
            .find_section(name)
            .map(Some)
            .ok_or_else(|| missing_section(doc, name));
    }
    let Some(fragment) = fragment else {
        return Ok(None);
    };
    let anchor = percent_decode_str(fragment).decode_utf8_lossy();
    let selected = doc.find_anchor(&anchor);
    if citation && selected.is_none() {
        return Err(missing_section(doc, &anchor));
    }
    Ok(selected)
}

fn read_passages(doc: &Document, uri: &str, req: &ArticleRequest) -> Result<Vec<Passage>> {
    let sections: Vec<_> = req
        .sections
        .iter()
        .map(|name| {
            doc.find_section(name)
                .ok_or_else(|| missing_section(doc, name))
        })
        .collect::<Result<_>>()?;
    let mut limits: Vec<_> = sections
        .iter()
        .map(|&i| {
            doc.section_markdown(i)
                .unwrap_or_default()
                .chars()
                .count()
                .saturating_sub(req.offset)
        })
        .collect();
    // Allocate short passages first so their unused share is available to longer ones.
    let mut order: Vec<_> = (0..limits.len()).collect();
    order.sort_by_key(|&i| limits[i]);
    let mut remaining = req.max_chars;
    for (n, i) in order.into_iter().enumerate() {
        limits[i] = limits[i].min(remaining.div_ceil(limits.len() - n));
        remaining -= limits[i];
    }
    let mut passages = Vec::with_capacity(sections.len());
    for (i, limit) in sections.into_iter().zip(limits) {
        let (content, total_chars, next_offset) = char_window(
            doc.section_markdown(i).unwrap_or_default(),
            req.offset,
            limit,
        );
        let section = &doc.sections[i];
        passages.push(Passage {
            section: section.title.clone(),
            uri: section_uri(uri, &section.anchor),
            content,
            offset: req.offset,
            total_chars,
            next_offset,
        });
    }
    Ok(passages)
}

fn raw_article(
    slot: &crate::Slot,
    resolved: crate::Resolved,
    req: &ArticleRequest,
    uri: String,
    has_fragment: bool,
) -> Result<ArticleResponse> {
    if req.section.is_some() || has_fragment || !req.sections.is_empty() {
        return Err(Error::Invalid(
            "section reads return Markdown; use an article path for raw HTML".into(),
        ));
    }
    let data = slot.archive().item_data(&resolved.dirent)?;
    let html = String::from_utf8_lossy(&data);
    let (content, total, next) = char_window(&html, req.offset, req.max_chars);
    Ok(ArticleResponse {
        archive: slot.name().to_string(),
        path: resolved.path,
        title: resolved.dirent.title().to_string(),
        uri,
        format: Format::Html,
        content,
        passages: Vec::new(),
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
    })
}

impl Library {
    pub fn read_article(&self, req: &ArticleRequest) -> Result<ArticleResponse> {
        let uri_archive = req
            .path
            .trim()
            .strip_prefix("zim://")
            .map(|s| s.split('/').next().unwrap_or(s));
        let archive = uri_archive.unwrap_or(&req.archive);
        let idx = self
            .slot_index(archive)
            .ok_or_else(|| Error::NoSuchArchive(archive.to_string()))?;
        if uri_archive.is_some()
            && !req.archive.is_empty()
            && self.slot_index(&req.archive) != Some(idx)
        {
            return Err(Error::Invalid(
                "archive differs from the citation's archive".into(),
            ));
        }
        let slot = &self.slots()[idx];
        let resolved = slot.resolve_entry(&req.path)?;
        let max_chars = req.max_chars;
        let uri = slot.uri(&resolved.path);
        let fragment = req
            .path
            .trim()
            .split_once('#')
            .map(|(_, f)| f)
            .filter(|f| !f.is_empty());
        if !req.sections.is_empty() && (req.section.is_some() || fragment.is_some()) {
            return Err(Error::Invalid(
                "use sections with an article path, or read one section".into(),
            ));
        }

        if req.format == Format::Html {
            return raw_article(slot, resolved, req, uri, fragment.is_some());
        }

        let doc = self.document(idx, &resolved.dirent)?;
        let selected = selected_section(
            &doc,
            req.section.as_deref(),
            fragment,
            uri_archive.is_some(),
        )?;
        let passages = read_passages(&doc, &uri, req)?;
        let (content, total, next) = if passages.is_empty() {
            let body = match selected {
                Some(i) => doc.section_markdown(i).unwrap_or_default(),
                None if req.format == Format::Text => &doc.text,
                None => &doc.markdown,
            };
            char_window(body, req.offset, max_chars)
        } else {
            (
                String::new(),
                passages.iter().map(|p| p.total_chars).sum(),
                None,
            )
        };
        let truncated = next.is_some() || passages.iter().any(|p| p.next_offset.is_some());
        let outline = (next.is_some() && selected.is_none()).then(|| outline_items(&doc, &uri));
        Ok(ArticleResponse {
            archive: slot.name().to_string(),
            path: resolved.path,
            title: if doc.title.is_empty() {
                resolved.dirent.title().to_string()
            } else {
                doc.title.clone()
            },
            uri: selected.map_or_else(
                || uri.clone(),
                |i| section_uri(&uri, &doc.sections[i].anchor),
            ),
            format: if selected.is_some() || !passages.is_empty() {
                Format::Markdown
            } else {
                req.format
            },
            content,
            passages,
            offset: req.offset,
            next_offset: next,
            total_chars: total,
            truncated,
            word_count: Some(doc.word_count),
            outline,
            links_count: Some(
                doc.internal_links(slot.archive().uses_new_namespace_scheme())
                    .len(),
            ),
            section: selected.map(|i| doc.sections[i].title.clone()),
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
        let uri = slot.uri(&resolved.path);
        Ok(OutlineResponse {
            archive: slot.name().to_string(),
            sections: outline_items(&doc, &uri),
            uri,
            path: resolved.path,
            title: doc.title.clone(),
            word_count: doc.word_count,
            total_chars: doc.markdown.chars().count(),
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
    fn batch_passages_share_budget_in_requested_order_and_resume() {
        let opts = zimz_extract::RenderOptions {
            base_namespace: b'C',
            base_path: "page",
            new_scheme: true,
            link_prefix: None,
            image_sources: false,
        };
        let doc = zimz_extract::extract_html(
            "<h2 id='part#1'>First</h2><p>一二三四五</p><h2 id='café'>Second</h2><p>ééééé</p>",
            zimz_extract::Adapter::Generic,
            None,
            &opts,
        );
        let req: ArticleRequest = serde_json::from_value(serde_json::json!({
            "path": "zim://wiki/page", "sections": ["Second", "First"], "max_chars": 23,
        }))
        .unwrap();
        let passages = read_passages(&doc, &req.path, &req).unwrap();
        assert_eq!(passages.len(), 2);
        assert_eq!(passages[0].uri, "zim://wiki/page#caf%C3%A9");
        assert_eq!(passages[1].uri, "zim://wiki/page#part%231");
        assert_eq!(
            passages
                .iter()
                .map(|p| p.content.chars().count())
                .sum::<usize>(),
            23
        );
        let total = passages.iter().map(|p| p.total_chars).sum();
        let complete = read_passages(
            &doc,
            &req.path,
            &ArticleRequest {
                max_chars: total,
                ..req.clone()
            },
        )
        .unwrap();
        assert!(complete.iter().all(|p| p.next_offset.is_none()));
        for p in passages {
            let body = doc
                .section_markdown(doc.find_section(&p.section).unwrap())
                .unwrap();
            let (rest, _, _) = char_window(body, p.next_offset.unwrap(), usize::MAX);
            assert_eq!(p.content + &rest, body);
        }
    }

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
