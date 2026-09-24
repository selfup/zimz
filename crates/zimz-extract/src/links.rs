// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Resolve `href`s found in stored HTML to ZIM entries. Stored paths are raw UTF-8 while
//! hrefs are percent-encoded and relative to the page (`../-/style.css`, `./Foo`,
//! `Definition%3AAngle`), with `?query` and `#fragment` parts that are not part of a path.

use percent_encoding::percent_decode_str;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Another entry of the same archive.
    Internal {
        namespace: u8,
        path: String,
        fragment: Option<String>,
    },
    /// Anything with a scheme (`https:`, `mailto:`), or protocol-relative `//host/…`.
    External(String),
    /// A fragment of the current page.
    Anchor(String),
}

impl LinkTarget {
    /// The path as libzim reports it: bare in new-scheme archives, `ns/path` in old ones.
    pub fn user_path(&self, new_scheme: bool) -> Option<String> {
        match self {
            LinkTarget::Internal {
                namespace, path, ..
            } => Some(if new_scheme {
                path.clone()
            } else {
                format!("{}/{path}", *namespace as char)
            }),
            _ => None,
        }
    }
}

fn has_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    for c in chars {
        if c == ':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return false;
        }
    }
    false
}

/// Join and normalise `.`/`..` segments of a path that starts with a namespace segment.
fn normalise(segments: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for seg in segments {
        match *seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s.to_string()),
        }
    }
    out
}

/// Resolve `href` relative to the entry `base_namespace/base_path`.
///
/// Returns `None` for empty, `javascript:` and `data:` links.
pub fn resolve_href(base_namespace: u8, base_path: &str, href: &str) -> Option<LinkTarget> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    if let Some(frag) = href.strip_prefix('#') {
        return Some(LinkTarget::Anchor(
            percent_decode_str(frag).decode_utf8_lossy().into_owned(),
        ));
    }
    if href.starts_with("//") {
        return Some(LinkTarget::External(format!("https:{href}")));
    }
    if has_scheme(href) {
        let lower = href.to_ascii_lowercase();
        if lower.starts_with("javascript:") || lower.starts_with("data:") {
            return None;
        }
        return Some(LinkTarget::External(href.to_string()));
    }
    let (rest, fragment) = match href.split_once('#') {
        Some((r, f)) => (
            r,
            Some(percent_decode_str(f).decode_utf8_lossy().into_owned()),
        ),
        None => (href, None),
    };
    let rest = rest.split_once('?').map_or(rest, |(r, _)| r);
    let decoded = percent_decode_str(rest).decode_utf8_lossy().into_owned();
    let ns_char = base_namespace as char;
    let resolved: Vec<String> = if let Some(abs) = decoded.strip_prefix('/') {
        // absolute paths are forbidden by the spec; treat them as relative to the namespace
        let mut v = vec![ns_char.to_string()];
        v.extend(normalise(&abs.split('/').collect::<Vec<_>>()));
        v
    } else {
        let mut segs: Vec<&str> = vec![];
        segs.push(""); // placeholder for the namespace
        let dir_end = base_path.rfind('/').unwrap_or(0);
        segs.extend(base_path[..dir_end].split('/').filter(|s| !s.is_empty()));
        segs.extend(decoded.split('/'));
        let ns_string = ns_char.to_string();
        let mut full: Vec<&str> = vec![ns_string.as_str()];
        full.extend(segs.into_iter().skip(1));
        normalise(&full)
    };
    if resolved.is_empty() {
        return None;
    }
    // if `..` climbed out of the namespace, the first segment names another namespace
    // (old scheme: `../-/style.css`, `../I/logo.png`); otherwise it is the base namespace
    let (namespace, path_segments) = if resolved[0].len() == 1 {
        (resolved[0].as_bytes()[0], &resolved[1..])
    } else {
        (base_namespace, &resolved[..])
    };
    let path = path_segments.join("/");
    if path.is_empty() {
        return None;
    }
    Some(LinkTarget::Internal {
        namespace,
        path,
        fragment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_wraps)]
    fn internal(ns: u8, path: &str) -> Option<LinkTarget> {
        Some(LinkTarget::Internal {
            namespace: ns,
            path: path.into(),
            fragment: None,
        })
    }

    #[test]
    fn relative_links_in_new_scheme() {
        assert_eq!(resolve_href(b'C', "Zstd", "ZSNES"), internal(b'C', "ZSNES"));
        assert_eq!(resolve_href(b'C', "Zstd", "./Foo"), internal(b'C', "Foo"));
        assert_eq!(
            resolve_href(b'C', "dir/page", "other"),
            internal(b'C', "dir/other")
        );
        assert_eq!(
            resolve_href(b'C', "dir/page", "../top"),
            internal(b'C', "top")
        );
        assert_eq!(
            resolve_href(b'C', "dir/page", "../../escaped"),
            internal(b'C', "escaped"),
            "cannot climb above the namespace"
        );
        assert_eq!(
            resolve_href(b'C', "Pythagoras", "Definition%3AAngle"),
            internal(b'C', "Definition:Angle")
        );
        assert_eq!(
            resolve_href(b'C', "www.example.com/a/b", "../c?x=1#frag"),
            Some(LinkTarget::Internal {
                namespace: b'C',
                path: "www.example.com/c".into(),
                fragment: Some("frag".into())
            })
        );
        assert_eq!(
            resolve_href(b'C', "page", "/absolute/x"),
            internal(b'C', "absolute/x")
        );
    }

    #[test]
    fn old_scheme_namespaces() {
        assert_eq!(
            resolve_href(b'A', "Acute_chest_pain", "STEMI"),
            internal(b'A', "STEMI")
        );
        assert_eq!(
            resolve_href(b'A', "Acute_chest_pain", "../-/style.css"),
            internal(b'-', "style.css")
        );
        assert_eq!(
            resolve_href(b'A', "Acute_chest_pain", "../I/m/logo.png"),
            internal(b'I', "m/logo.png")
        );
        assert_eq!(
            LinkTarget::Internal {
                namespace: b'A',
                path: "X".into(),
                fragment: None
            }
            .user_path(false),
            Some("A/X".into())
        );
        assert_eq!(
            LinkTarget::Internal {
                namespace: b'C',
                path: "X".into(),
                fragment: None
            }
            .user_path(true),
            Some("X".into())
        );
    }

    #[test]
    fn external_anchor_and_junk() {
        assert_eq!(
            resolve_href(b'C', "p", "https://en.wikipedia.org/wiki/Zstd"),
            Some(LinkTarget::External(
                "https://en.wikipedia.org/wiki/Zstd".into()
            ))
        );
        assert_eq!(
            resolve_href(b'C', "p", "mailto:x@y.z"),
            Some(LinkTarget::External("mailto:x@y.z".into()))
        );
        assert_eq!(
            resolve_href(b'C', "p", "//cdn.example.com/x.js"),
            Some(LinkTarget::External("https://cdn.example.com/x.js".into()))
        );
        assert_eq!(
            resolve_href(b'C', "p", "#Features"),
            Some(LinkTarget::Anchor("Features".into()))
        );
        assert_eq!(resolve_href(b'C', "p", "javascript:void(0)"), None);
        assert_eq!(resolve_href(b'C', "p", "data:image/png;base64,AAAA"), None);
        assert_eq!(resolve_href(b'C', "p", "   "), None);
        assert_eq!(
            resolve_href(b'C', "p", "Definition:Angle"),
            Some(LinkTarget::External("Definition:Angle".into())),
            "a colon makes it look like a scheme; scrapers percent-encode it (%3A) to avoid this"
        );
    }
}
