// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! DOM → Markdown or plain text, recording headings and links on the way.

use std::fmt::Write as _;

use scraper::node::Node;
use scraper::{ElementRef, Html, Selector};

use crate::document::{Link, Section};
use crate::links::{LinkTarget, resolve_href};

#[derive(Debug, Clone)]
pub struct RenderOptions<'a> {
    /// Namespace and path of the entry being rendered (for relative links).
    pub base_namespace: u8,
    pub base_path: &'a str,
    pub new_scheme: bool,
    /// Prefix for internal link targets, e.g. `zim://wikipedia/`; `None` keeps bare paths.
    pub link_prefix: Option<&'a str>,
    /// Emit `![alt](src)` instead of `![alt]`.
    pub image_sources: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Rendered {
    pub output: String,
    pub sections: Vec<Section>,
    pub links: Vec<Link>,
}

const SKIP: &[&str] = &[
    "script", "style", "noscript", "template", "head", "title", "link", "meta", "iframe", "object",
    "embed", "svg", "canvas", "button", "input", "select", "textarea", "audio", "video", "source",
    "track", "map", "area", "dialog", "menu",
];

const BLOCK: &[&str] = &[
    "p", "div", "section", "article", "main", "aside", "header", "footer", "nav", "address",
    "figure", "center", "fieldset", "details", "body", "html", "hgroup", "form",
];

struct Renderer<'o> {
    md: bool,
    opts: &'o RenderOptions<'o>,
    out: String,
    sections: Vec<Section>,
    links: Vec<Link>,
    list_stack: Vec<Option<u32>>, // None = bullet, Some(n) = next number
    blockquote_depth: usize,
    pre_depth: usize,
    inline_prefix_pending: bool,
}

fn attr<'a>(el: &ElementRef<'a>, name: &str) -> Option<&'a str> {
    el.value().attr(name)
}

fn has_class(el: &ElementRef<'_>, class: &str) -> bool {
    el.value().classes().any(|c| c == class)
}

const BLOCKISH: &[&str] = &[
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "p",
    "div",
    "section",
    "article",
    "ul",
    "ol",
    "li",
    "table",
    "pre",
    "blockquote",
    "dl",
    "hr",
    "figure",
    "details",
];

/// Inline wrappers (`<a>`, `<b>`, `<summary>`, …) around block content are rendered as
/// plain containers so headings and lists inside them keep their structure.
fn contains_block(el: ElementRef<'_>) -> bool {
    el.descendants()
        .filter_map(ElementRef::wrap)
        .any(|d| BLOCKISH.contains(&d.value().name()))
}

impl Renderer<'_> {
    fn at_line_start(&self) -> bool {
        self.out.is_empty() || self.out.ends_with('\n')
    }

    fn ensure_newline(&mut self) {
        if !self.at_line_start() {
            self.out.push('\n');
        }
    }

    fn ensure_blank_line(&mut self) {
        self.ensure_newline();
        if !(self.out.is_empty() || self.out.ends_with("\n\n")) {
            self.out.push('\n');
        }
    }

    fn line_prefix(&self) -> String {
        let mut p = String::new();
        for _ in 0..self.blockquote_depth {
            p.push_str("> ");
        }
        p
    }

    fn start_line(&mut self) {
        if self.at_line_start() {
            let p = self.line_prefix();
            self.out.push_str(&p);
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.pre_depth > 0 {
            self.out.push_str(text);
            return;
        }
        let mut collapsed = String::with_capacity(text.len());
        let mut last_space = self.at_line_start() || self.out.ends_with(' ');
        for c in text.chars() {
            if c.is_whitespace() {
                if !last_space {
                    collapsed.push(' ');
                    last_space = true;
                }
            } else {
                collapsed.push(c);
                last_space = false;
            }
        }
        if collapsed.is_empty() || (self.at_line_start() && collapsed == " ") {
            return;
        }
        if self.at_line_start() {
            self.start_line();
            self.out.push_str(collapsed.trim_start());
        } else {
            self.out.push_str(&collapsed);
        }
    }

    fn render_children(&mut self, el: ElementRef<'_>) {
        for child in el.children() {
            match child.value() {
                Node::Text(t) => self.push_text(t),
                Node::Element(_) => {
                    if let Some(e) = ElementRef::wrap(child) {
                        self.render_element(e);
                    }
                }
                _ => {}
            }
        }
    }

    /// Render children into a separate buffer (for wrappers like `[…](…)` and `**…**`).
    fn capture(&mut self, el: ElementRef<'_>) -> String {
        let saved = std::mem::take(&mut self.out);
        let depth = std::mem::take(&mut self.blockquote_depth);
        self.render_children(el);
        self.blockquote_depth = depth;
        let inner = std::mem::replace(&mut self.out, saved);
        inner.trim().to_string()
    }

    /// Whitespace-collapsed plain text of an element.
    fn plain_text(el: ElementRef<'_>) -> String {
        el.text()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn inline_wrap(&mut self, el: ElementRef<'_>, open: &str, close: &str) {
        if contains_block(el) {
            self.render_children(el);
            return;
        }
        let inner = self.capture(el);
        if inner.is_empty() {
            return;
        }
        self.start_line();
        if self.md {
            // markdown emphasis cannot span whitespace at the edges
            self.out.push_str(open);
            self.out.push_str(&inner);
            self.out.push_str(close);
        } else {
            self.out.push_str(&inner);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn render_element(&mut self, el: ElementRef<'_>) {
        let name = el.value().name();
        if SKIP.contains(&name) || attr(&el, "hidden").is_some() {
            return;
        }
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name.as_bytes()[1] - b'0';
                let title = self.capture(el);
                if title.is_empty() {
                    return;
                }
                self.ensure_blank_line();
                let start = self.out.len();
                self.start_line();
                if self.md {
                    for _ in 0..level {
                        self.out.push('#');
                    }
                    self.out.push(' ');
                }
                self.out.push_str(&title);
                self.out.push('\n');
                if !self.md {
                    self.out.push('\n');
                }
                self.sections.push(Section {
                    level,
                    title,
                    start,
                    end: 0,
                });
            }
            "br" => {
                if self.pre_depth == 0 {
                    self.out.push('\n');
                }
            }
            "hr" => {
                self.ensure_blank_line();
                if self.md {
                    self.start_line();
                    self.out.push_str("---\n\n");
                }
            }
            "pre" => {
                let text = el.text().collect::<String>();
                let text = text.trim_matches('\n');
                if text.trim().is_empty() {
                    return;
                }
                let lang = el
                    .select(&Selector::parse("code").unwrap())
                    .next()
                    .and_then(|c| {
                        c.value()
                            .classes()
                            .find(|c| c.starts_with("language-"))
                            .map(|c| c["language-".len()..].to_string())
                    })
                    .or_else(|| {
                        el.value()
                            .classes()
                            .find(|c| c.starts_with("language-"))
                            .map(|c| c["language-".len()..].to_string())
                    })
                    .unwrap_or_default();
                self.ensure_blank_line();
                let prefix = self.line_prefix();
                if self.md {
                    self.out.push_str(&prefix);
                    self.out.push_str("```");
                    self.out.push_str(&lang);
                    self.out.push('\n');
                }
                for line in text.lines() {
                    self.out.push_str(&prefix);
                    self.out.push_str(line.trim_end());
                    self.out.push('\n');
                }
                if self.md {
                    self.out.push_str(&prefix);
                    self.out.push_str("```\n");
                }
                self.out.push('\n');
            }
            "blockquote" => {
                self.ensure_blank_line();
                self.blockquote_depth += 1;
                self.render_children(el);
                self.blockquote_depth -= 1;
                self.ensure_blank_line();
            }
            "ul" | "ol" => {
                if self.list_stack.is_empty() {
                    self.ensure_blank_line();
                } else {
                    self.ensure_newline();
                }
                let start: u32 = attr(&el, "start").and_then(|s| s.parse().ok()).unwrap_or(1);
                self.list_stack
                    .push(if name == "ol" { Some(start) } else { None });
                for child in el.children() {
                    if let Some(li) = ElementRef::wrap(child) {
                        if li.value().name() == "li" {
                            self.render_list_item(li);
                        } else {
                            self.render_element(li);
                        }
                    }
                }
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.ensure_blank_line();
                }
            }
            "li" => self.render_list_item(el),
            "dl" => {
                self.ensure_blank_line();
                self.render_children(el);
                self.ensure_blank_line();
            }
            "dt" => {
                let inner = self.capture(el);
                if inner.is_empty() {
                    return;
                }
                self.ensure_newline();
                self.start_line();
                if self.md {
                    self.out.push_str("**");
                    self.out.push_str(&inner);
                    self.out.push_str("**\n");
                } else {
                    self.out.push_str(&inner);
                    self.out.push('\n');
                }
            }
            "dd" => {
                let inner = self.capture(el);
                if inner.is_empty() {
                    return;
                }
                self.ensure_newline();
                self.start_line();
                for (i, line) in inner.lines().enumerate() {
                    if i > 0 {
                        self.out.push('\n');
                        self.start_line();
                    }
                    if self.md {
                        self.out.push_str("  ");
                    }
                    self.out.push_str(line);
                }
                self.out.push('\n');
            }
            "table" => self.render_table(el),
            "img" => {
                let alt = attr(&el, "alt").map_or("", str::trim);
                if alt.is_empty() {
                    return;
                }
                self.start_line();
                if self.md {
                    self.out.push_str("![");
                    self.out.push_str(alt);
                    self.out.push(']');
                    if self.opts.image_sources
                        && let Some(src) = attr(&el, "src")
                    {
                        self.out.push('(');
                        self.out.push_str(src);
                        self.out.push(')');
                    }
                } else {
                    self.out.push_str(alt);
                }
            }
            "a" => {
                let target = attr(&el, "href")
                    .and_then(|h| resolve_href(self.opts.base_namespace, self.opts.base_path, h));
                if contains_block(el) {
                    if let Some(t) = target {
                        self.links.push(Link {
                            text: Self::plain_text(el),
                            target: t,
                        });
                    }
                    self.render_children(el);
                    return;
                }
                let inner = self.capture(el);
                if inner.is_empty() {
                    return;
                }
                self.start_line();
                match (&target, self.md) {
                    (Some(t), true) => {
                        let dest = match t {
                            LinkTarget::Internal { fragment, .. } => {
                                let p = t.user_path(self.opts.new_scheme).unwrap_or_default();
                                let mut d = format!("{}{p}", self.opts.link_prefix.unwrap_or(""));
                                if let Some(f) = fragment {
                                    d.push('#');
                                    d.push_str(f);
                                }
                                d
                            }
                            LinkTarget::External(u) => u.clone(),
                            LinkTarget::Anchor(f) => format!("#{f}"),
                        };
                        self.out.push('[');
                        self.out.push_str(&inner);
                        self.out.push_str("](");
                        self.out.push_str(&dest.replace(' ', "%20"));
                        self.out.push(')');
                    }
                    _ => self.out.push_str(&inner),
                }
                if let Some(t) = target {
                    self.links.push(Link {
                        text: Self::plain_text(el),
                        target: t,
                    });
                }
            }
            "strong" | "b" => self.inline_wrap(el, "**", "**"),
            "em" | "i" | "cite" | "dfn" | "var" => self.inline_wrap(el, "*", "*"),
            "s" | "del" | "strike" => self.inline_wrap(el, "~~", "~~"),
            "code" | "kbd" | "samp" | "tt" => {
                if self.pre_depth > 0 {
                    self.render_children(el);
                } else {
                    self.inline_wrap(el, "`", "`");
                }
            }
            "math" => {
                let tex = attr(&el, "alttext").map(str::to_string).or_else(|| {
                    el.select(&Selector::parse("annotation").unwrap())
                        .next()
                        .map(|a| a.text().collect::<String>())
                });
                if let Some(t) = tex {
                    let t = t.trim();
                    if !t.is_empty() {
                        self.start_line();
                        if self.md {
                            self.out.push('$');
                            self.out.push_str(t);
                            self.out.push('$');
                        } else {
                            self.out.push_str(t);
                        }
                    }
                }
            }
            "summary" => {
                if contains_block(el) {
                    self.render_children(el);
                    return;
                }
                let inner = self.capture(el);
                if !inner.is_empty() {
                    self.ensure_blank_line();
                    self.start_line();
                    if self.md {
                        self.out.push_str("**");
                        self.out.push_str(&inner);
                        self.out.push_str("**\n\n");
                    } else {
                        self.out.push_str(&inner);
                        self.out.push_str("\n\n");
                    }
                }
            }
            "figcaption" | "caption" => {
                let inner = self.capture(el);
                if !inner.is_empty() {
                    self.ensure_blank_line();
                    self.start_line();
                    if self.md {
                        self.out.push('*');
                        self.out.push_str(&inner);
                        self.out.push_str("*\n\n");
                    } else {
                        self.out.push_str(&inner);
                        self.out.push_str("\n\n");
                    }
                }
            }
            "tr" | "td" | "th" | "thead" | "tbody" | "tfoot" => {
                // stray table parts outside a table: render inline
                self.render_children(el);
            }
            n if BLOCK.contains(&n) => {
                self.ensure_blank_line();
                self.render_children(el);
                self.ensure_blank_line();
            }
            _ => self.render_children(el),
        }
    }

    fn render_list_item(&mut self, li: ElementRef<'_>) {
        let depth = self.list_stack.len().saturating_sub(1);
        let marker = match self.list_stack.last_mut() {
            Some(Some(n)) => {
                let m = format!("{n}. ");
                *n += 1;
                m
            }
            _ => "- ".to_string(),
        };
        self.ensure_newline();
        self.start_line();
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        if self.md || depth > 0 {
            self.out.push_str(&marker);
        } else {
            self.out.push_str("- ");
        }
        let before = self.out.len();
        self.render_children(li);
        if self.out.len() == before {
            // empty item
            self.out.truncate(before);
            self.out.truncate(
                self.out
                    .trim_end_matches(|c: char| {
                        c == ' ' || c == '-' || c == '.' || c.is_ascii_digit()
                    })
                    .len(),
            );
        }
        self.ensure_newline();
    }

    fn cell_text(&mut self, cell: ElementRef<'_>) -> String {
        let inner = self.capture(cell);
        inner
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace('|', "\\|")
    }

    fn render_table(&mut self, table: ElementRef<'_>) {
        let row_sel = Selector::parse("tr").unwrap();
        let rows: Vec<ElementRef<'_>> = table
            .select(&row_sel)
            .filter(|r| {
                r.ancestors()
                    .filter_map(ElementRef::wrap)
                    .find(|a| a.value().name() == "table")
                    .is_some_and(|t| t.id() == table.id())
            })
            .collect();
        if rows.is_empty() {
            self.render_children(table);
            return;
        }
        self.ensure_blank_line();
        if let Some(cap) = table.select(&Selector::parse("caption").unwrap()).next() {
            let c = self.capture(cap);
            if !c.is_empty() {
                self.start_line();
                if self.md {
                    let _ = write!(self.out, "*{c}*\n\n");
                } else {
                    let _ = write!(self.out, "{c}\n\n");
                }
            }
        }
        // infobox-style tables (one label + one value per row) read better as a list
        let infobox = has_class(&table, "infobox")
            || has_class(&table, "wikitable-infobox")
            || attr(&table, "class").is_some_and(|c| c.contains("infobox"));
        let mut header_done = false;
        for row in rows {
            let cells: Vec<ElementRef<'_>> = row
                .children()
                .filter_map(ElementRef::wrap)
                .filter(|c| matches!(c.value().name(), "td" | "th"))
                .collect();
            if cells.is_empty() {
                continue;
            }
            let texts: Vec<String> = cells.iter().map(|c| self.cell_text(*c)).collect();
            if texts.iter().all(String::is_empty) {
                continue;
            }
            if infobox {
                self.start_line();
                if texts.len() == 2 && cells.first().is_some_and(|c| c.value().name() == "th") {
                    if self.md {
                        let _ = writeln!(self.out, "- **{}:** {}", texts[0], texts[1]);
                    } else {
                        let _ = writeln!(self.out, "{}: {}", texts[0], texts[1]);
                    }
                } else {
                    let joined = texts
                        .iter()
                        .filter(|t| !t.is_empty())
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" — ");
                    if self.md {
                        let _ = writeln!(self.out, "- {joined}");
                    } else {
                        let _ = writeln!(self.out, "{joined}");
                    }
                }
                continue;
            }
            self.start_line();
            if self.md {
                self.out.push_str("| ");
                self.out.push_str(&texts.join(" | "));
                self.out.push_str(" |\n");
                if !header_done {
                    self.start_line();
                    self.out.push('|');
                    for _ in &texts {
                        self.out.push_str("---|");
                    }
                    self.out.push('\n');
                }
            } else {
                self.out.push_str(&texts.join(" | "));
                self.out.push('\n');
            }
            header_done = true;
        }
        self.out.push('\n');
    }
}

/// Render `root` (an element) to Markdown (`markdown = true`) or plain text.
pub fn render(root: ElementRef<'_>, opts: &RenderOptions<'_>, markdown: bool) -> Rendered {
    let mut r = Renderer {
        md: markdown,
        opts,
        out: String::new(),
        sections: Vec::new(),
        links: Vec::new(),
        list_stack: Vec::new(),
        blockquote_depth: 0,
        pre_depth: 0,
        inline_prefix_pending: false,
    };
    let _ = r.inline_prefix_pending;
    r.render_element(root);
    let mut output = r.out;
    // collapse runs of blank lines and trailing whitespace per line
    let mut cleaned = String::with_capacity(output.len());
    let mut blank = 0;
    for line in output.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        cleaned.push_str(line);
        cleaned.push('\n');
    }
    output = cleaned.trim().to_string();
    output.push('\n');
    // section ranges: re-locate headings in the cleaned output by walking heading lines
    let mut sections = r.sections;
    relocate_sections(&mut sections, &output, markdown);
    Rendered {
        output,
        sections,
        links: r.links,
    }
}

fn relocate_sections(sections: &mut Vec<Section>, output: &str, markdown: bool) {
    let mut pos = 0usize;
    let mut i = 0usize;
    while i < sections.len() {
        let s = &sections[i];
        let needle = if markdown {
            format!("{} {}\n", "#".repeat(usize::from(s.level)), s.title)
        } else {
            format!("{}\n", s.title)
        };
        let line_start = output[pos..]
            .find(&needle)
            .map(|off| pos + off)
            .and_then(|p| (p == 0 || output.as_bytes()[p - 1] == b'\n').then_some(p));
        match line_start {
            Some(p) => {
                sections[i].start = p;
                pos = p + needle.len();
                i += 1;
            }
            None => {
                sections.remove(i);
            }
        }
    }
    let n = sections.len();
    for i in 0..n {
        let level = sections[i].level;
        let end = sections[i + 1..]
            .iter()
            .find(|s| s.level <= level)
            .map_or(output.len(), |s| s.start);
        sections[i].end = end;
    }
}

/// Parse HTML, remove `prune` selectors, and pick the first matching `content` selector.
pub fn prepare(html: &str, content: &[&str], prune: &[&str]) -> Html {
    let mut doc = Html::parse_document(html);
    let mut to_remove = Vec::new();
    for sel in prune {
        if let Ok(s) = Selector::parse(sel) {
            to_remove.extend(doc.select(&s).map(|e| e.id()));
        }
    }
    for id in to_remove {
        if let Some(mut node) = doc.tree.get_mut(id) {
            node.detach();
        }
    }
    let _ = content;
    doc
}

/// The element to render: first selector that matches, else `<body>`, else the root.
pub fn content_root<'a>(doc: &'a Html, content: &[&str]) -> ElementRef<'a> {
    for sel in content {
        if let Ok(s) = Selector::parse(sel)
            && let Some(el) = doc.select(&s).next()
        {
            return el;
        }
    }
    if let Ok(s) = Selector::parse("body")
        && let Some(el) = doc.select(&s).next()
    {
        return el;
    }
    doc.root_element()
}

/// `<title>` text, if any.
pub fn html_title(doc: &Html) -> Option<String> {
    let s = Selector::parse("title").ok()?;
    let t = doc.select(&s).next()?.text().collect::<String>();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    (!t.is_empty()).then_some(t)
}
