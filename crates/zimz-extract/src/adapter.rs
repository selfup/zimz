// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Per-scraper knowledge: where the article body lives, what to prune, and how to reach
//! the JSON records that app-style scrapers store instead of HTML.

use std::fmt::Write as _;

use zimz_core::{Archive, Dirent};

use crate::document::Document;
use crate::render::{RenderOptions, content_root, html_title, prepare, render};
use crate::vtt;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Adapter {
    /// MediaWiki sites via mwoffliner (Wikipedia, Wikispecies, ProofWiki, WikEM, …).
    MwOffliner,
    /// devdocs2zim.
    DevDocs,
    /// warc2zim / zimit website captures.
    Zimit,
    /// gutenberg2zim books.
    Gutenberg,
    /// mindtouch2zim (LibreTexts): stub pages + `content/page_content_<id>.json`.
    LibreTexts,
    /// youtube2zim: stub pages + `videos/<slug>.json` (+ subtitles).
    YouTube,
    /// Anything else: main-content heuristics.
    Generic,
}

impl Adapter {
    /// Pick an adapter from the `Scraper` metadata (and `Name` as a fallback hint).
    pub fn from_metadata(scraper: Option<&str>, name: Option<&str>) -> Self {
        let s = scraper.unwrap_or("").to_ascii_lowercase();
        let n = name.unwrap_or("").to_ascii_lowercase();
        if s.contains("mwoffliner") {
            Self::MwOffliner
        } else if s.contains("devdocs") {
            Self::DevDocs
        } else if s.contains("warc2zim") || s.contains("zimit") {
            Self::Zimit
        } else if s.contains("gutenberg") || n.starts_with("gutenberg") {
            Self::Gutenberg
        } else if s.contains("mindtouch") || n.contains("libretexts") {
            Self::LibreTexts
        } else if s.contains("youtube") {
            Self::YouTube
        } else if n.starts_with("wikipedia") || n.starts_with("wiki") {
            Self::MwOffliner
        } else {
            Self::Generic
        }
    }

    /// CSS selectors for the article body, tried in order.
    pub fn content_selectors(self) -> &'static [&'static str] {
        match self {
            Self::MwOffliner => &[
                "#mw-content-text .mw-parser-output",
                ".mw-parser-output",
                "#mw-content-text",
                "#bodyContent",
                "#content",
            ],
            Self::DevDocs => &["._page", "._content", "main"],
            Self::Gutenberg | Self::LibreTexts | Self::YouTube => &["body"],
            Self::Zimit | Self::Generic => &[
                "main",
                "article",
                "[role=main]",
                "#main-content",
                "#main",
                "#content",
                ".content",
                "#page",
                "body",
            ],
        }
    }

    /// Selectors removed before rendering.
    pub fn prune_selectors(self) -> &'static [&'static str] {
        match self {
            Self::MwOffliner => &[
                ".navbox",
                ".vertical-navbox",
                ".navbox-styles",
                ".reflist",
                ".references",
                "ol.references",
                ".mw-references-wrap",
                ".mw-references",
                "sup.reference",
                "sup.mw-ref",
                ".mw-ref",
                ".mw-reflink-text",
                ".mw-cite-backlink",
                ".mw-editsection",
                ".hatnote",
                ".mw-empty-elt",
                ".noprint",
                ".metadata",
                ".ambox",
                ".mbox-small",
                ".sistersitebox",
                ".portalbox",
                ".portal-bar",
                "#toc",
                ".toc",
                ".mw-jump-link",
                ".zim-footer",
                "#mw-navigation",
                ".catlinks",
                ".printfooter",
                ".navigation-not-searchable",
                ".mw-authority-control",
                ".shortdescription",
                ".side-box",
                "#siteSub",
                "#contentSub",
                ".mw-indicators",
                ".vector-toc",
                ".mw-cite-backlink",
                ".error",
                "#References",
                "#External_links",
                ".external-links",
                ".navbox-inner",
            ],
            Self::DevDocs => &["devdocs-navbar", "._attribution", "nav", ".icon"],
            Self::Zimit | Self::Generic => &[
                "nav",
                "header",
                "footer",
                "aside",
                "#menu",
                ".menu",
                "#menu-fixed",
                ".burger",
                ".crumbs",
                ".breadcrumb",
                ".breadcrumbs",
                ".cookie",
                ".cookie-banner",
                ".sidebar",
                "#sidebar",
                ".share",
                ".social",
                ".comments",
                "#comments",
                "form",
                ".advertisement",
                ".ad",
                "#ta",
                ".skip-link",
                "[aria-hidden=true]",
            ],
            Self::Gutenberg => &[
                "#pg-header",
                "#pg-footer",
                ".pg-boilerplate",
                "#pg-machine-header",
                "#pg-start-separator",
                "#pg-end-separator",
                ".zim_info",
                ".zim_epub",
                ".zim_up",
                ".pagenum",
                ".tocpg",
            ],
            Self::LibreTexts => &[".mt-icon-article", "nav", ".sr-only"],
            Self::YouTube => &[],
        }
    }
}

/// Guess the adapter for an archive from its metadata.
pub fn detect_adapter(archive: &Archive) -> Adapter {
    let scraper = archive.metadata_string("Scraper").ok().flatten();
    let name = archive.metadata_string("Name").ok().flatten();
    Adapter::from_metadata(scraper.as_deref(), name.as_deref())
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Render an HTML string with an adapter's rules.
pub fn extract_html(
    html: &str,
    adapter: Adapter,
    title_hint: Option<&str>,
    opts: &RenderOptions<'_>,
) -> Document {
    let doc = prepare(html, adapter.content_selectors(), adapter.prune_selectors());
    let root = content_root(&doc, adapter.content_selectors());
    let md = render(root, opts, true);
    let txt = render(root, opts, false);
    let title = html_title(&doc)
        .filter(|t| !t.is_empty())
        .or_else(|| md.sections.first().map(|s| s.title.clone()))
        .or_else(|| title_hint.map(str::to_string))
        .unwrap_or_default();
    Document {
        title,
        word_count: word_count(&txt.output),
        text: txt.output,
        sections: md.sections,
        links: md.links,
        markdown: md.output,
        adapter,
        source_path: opts.base_path.to_string(),
    }
}

fn read_string(archive: &Archive, ns: u8, path: &str) -> Result<Option<String>> {
    match archive.entry_by_path(ns, path)? {
        Some(d) => {
            let d = archive.resolve(&d)?;
            if d.is_item() {
                Ok(Some(
                    String::from_utf8_lossy(&archive.item_data(&d)?).into_owned(),
                ))
            } else {
                Ok(None)
            }
        }
        None => Ok(None),
    }
}

/// Extract an entry. `link_prefix` becomes the prefix of internal link targets in the
/// Markdown (e.g. `zim://wikipedia_en_all/`).
pub fn extract(
    archive: &Archive,
    entry: &Dirent,
    adapter: Adapter,
    link_prefix: Option<&str>,
) -> Result<Document> {
    let entry = archive.resolve(&std::sync::Arc::new(entry.clone()))?;
    if !entry.is_item() {
        return Err(Error::Unsupported(entry.full_path(), "not an item".into()));
    }
    let mime = archive.mime_type(&entry).unwrap_or("").to_string();
    let opts = RenderOptions {
        base_namespace: entry.namespace,
        base_path: &entry.path,
        new_scheme: archive.uses_new_namespace_scheme(),
        link_prefix,
        image_sources: false,
    };

    // app-style scrapers: the HTML entry is a stub, the content is a JSON record
    match adapter {
        Adapter::LibreTexts => {
            if let Some(id) = entry.path.strip_prefix("index/page_") {
                let json_path = format!("content/page_content_{id}.json");
                if let Some(json) = read_string(archive, entry.namespace, &json_path)? {
                    let v: serde_json::Value = serde_json::from_str(&json)?;
                    let body = v.get("htmlBody").and_then(|b| b.as_str()).unwrap_or("");
                    let stub = String::from_utf8_lossy(&archive.item_data(&entry)?).into_owned();
                    let title = html_title(&prepare(&stub, &[], &[]))
                        .unwrap_or_else(|| entry.title().to_string());
                    let html = format!(
                        "<html><head><title>{}</title></head><body>{body}</body></html>",
                        html_escape(&title)
                    );
                    let mut doc = extract_html(&html, adapter, Some(&title), &opts);
                    doc.source_path = json_path;
                    doc.title = title;
                    return Ok(doc);
                }
            }
        }
        Adapter::YouTube => {
            if let Some(slug) = entry.path.strip_prefix("index/") {
                let json_path = format!("videos/{slug}.json");
                if let Some(json) = read_string(archive, entry.namespace, &json_path)? {
                    return youtube_document(archive, &entry, &json_path, &json, &opts);
                }
            }
        }
        _ => {}
    }

    if mime.starts_with("text/html") || mime.starts_with("application/xhtml") {
        let html = String::from_utf8_lossy(&archive.item_data(&entry)?).into_owned();
        let mut doc = extract_html(&html, adapter, Some(entry.title()), &opts);
        if doc.title.is_empty() {
            doc.title = entry.title().to_string();
        }
        return Ok(doc);
    }
    if mime.starts_with("text/plain") || mime.starts_with("text/markdown") {
        let text = String::from_utf8_lossy(&archive.item_data(&entry)?).into_owned();
        return Ok(Document {
            title: entry.title().to_string(),
            word_count: word_count(&text),
            markdown: text.clone(),
            text,
            sections: Vec::new(),
            links: Vec::new(),
            adapter,
            source_path: entry.path.clone(),
        });
    }
    Err(Error::Unsupported(entry.full_path(), mime))
}

/// Level-2 sections of synthesized Markdown (`## ` lines).
fn markdown_sections(md: &str) -> Vec<crate::document::Section> {
    let mut sections = Vec::new();
    let mut offset = 0usize;
    for line in md.split_inclusive('\n') {
        if let Some(t) = line.strip_prefix("## ") {
            sections.push(crate::document::Section {
                level: 2,
                title: t.trim_end().to_string(),
                start: offset,
                end: md.len(),
            });
        }
        offset += line.len();
    }
    for i in 0..sections.len() {
        if i + 1 < sections.len() {
            sections[i].end = sections[i + 1].start;
        }
    }
    sections
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn youtube_document(
    archive: &Archive,
    entry: &Dirent,
    json_path: &str,
    json: &str,
    opts: &RenderOptions<'_>,
) -> Result<Document> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let title = if s("title").is_empty() {
        entry.title().to_string()
    } else {
        s("title")
    };
    let mut md = format!("# {title}\n\n");
    let author = v
        .get("author")
        .and_then(|a| a.get("channelTitle"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let mut meta = Vec::new();
    if !author.is_empty() {
        meta.push(format!("**Channel:** {author}"));
    }
    if !s("publicationDate").is_empty() {
        meta.push(format!("**Published:** {}", s("publicationDate")));
    }
    if !s("duration").is_empty() {
        meta.push(format!("**Duration:** {}", s("duration")));
    }
    if !meta.is_empty() {
        md.push_str(&meta.join("  \n"));
        md.push_str("\n\n");
    }
    let description = s("description");
    if !description.is_empty() {
        md.push_str("## Description\n\n");
        md.push_str(description.trim());
        md.push_str("\n\n");
    }
    if let Some(chapters) = v.get("chapterList").and_then(|c| c.as_array())
        && chapters.len() > 1
    {
        md.push_str("## Chapters\n\n");
        for ch in chapters {
            let t = ch.get("title").and_then(|x| x.as_str()).unwrap_or("");
            let start = ch
                .get("startTime")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let _ = writeln!(md, "- {:02}:{:02} {t}", start / 60, start % 60);
        }
        md.push('\n');
    }
    // subtitles: videos/<id>/video.<lang>.vtt, English first
    let id = s("id");
    let mut langs: Vec<String> = v
        .get("subtitleList")
        .and_then(|l| l.as_array())
        .map(|l| {
            l.iter()
                .filter_map(|x| x.get("code").and_then(|c| c.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    langs.sort_by_key(|l| (!l.starts_with("en"), l.clone()));
    for lang in langs.into_iter().take(1) {
        let path = format!("videos/{id}/video.{lang}.vtt");
        if let Some(raw) = read_string(archive, entry.namespace, &path)? {
            let t = vtt::transcript(&raw);
            if !t.is_empty() {
                let _ = write!(md, "## Transcript ({lang})\n\n{t}\n");
            }
        }
    }
    let html = format!(
        "<html><head><title>{}</title></head><body></body></html>",
        html_escape(&title)
    );
    let mut doc = extract_html(&html, Adapter::YouTube, Some(&title), opts);
    doc.title = title;
    doc.text = md.replace("**", "").replace("## ", "").replace("# ", "");
    doc.word_count = word_count(&doc.text);
    doc.sections = markdown_sections(&md);
    doc.markdown = md;
    doc.source_path = json_path.to_string();
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_detection() {
        assert_eq!(
            Adapter::from_metadata(Some("mwoffliner 1.17.5"), None),
            Adapter::MwOffliner
        );
        assert_eq!(
            Adapter::from_metadata(Some("devdocs2zim v0.2.1"), None),
            Adapter::DevDocs
        );
        assert_eq!(
            Adapter::from_metadata(
                Some("warc2zim 2.3.0,Browsertrix-Crawler 1.11.2 (with warcio.js 2.4.9),zimit"),
                None
            ),
            Adapter::Zimit
        );
        assert_eq!(
            Adapter::from_metadata(Some("gutenberg2zim-3.0.1"), None),
            Adapter::Gutenberg
        );
        assert_eq!(
            Adapter::from_metadata(Some("mindtouch2zim v0.1.1"), None),
            Adapter::LibreTexts
        );
        assert_eq!(
            Adapter::from_metadata(Some("youtube2zim 3.5.0"), None),
            Adapter::YouTube
        );
        assert_eq!(
            Adapter::from_metadata(None, Some("gutenberg_en_lcc-q")),
            Adapter::Gutenberg
        );
        assert_eq!(
            Adapter::from_metadata(None, Some("ifixit_en_all")),
            Adapter::Generic
        );
        assert_eq!(Adapter::from_metadata(None, None), Adapter::Generic);
    }
}
