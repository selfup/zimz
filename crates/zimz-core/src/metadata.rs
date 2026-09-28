// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Helpers for the `M/` namespace: the `Counter` value and illustration names.

/// Parse `M/Counter` (`mime=count;mime=count;…`). MIME types may themselves contain
/// `;` and `=` parameters, so chunks without a trailing `=<digits>` are re-joined.
pub fn parse_counter(value: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut pending = String::new();
    for chunk in value.split(';') {
        if !pending.is_empty() {
            pending.push(';');
        }
        pending.push_str(chunk);
        if let Some((mime, count)) = pending.rsplit_once('=')
            && !count.is_empty()
            && !mime.is_empty()
            && count.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = count.parse::<u64>()
        {
            out.push((mime.to_string(), n));
            pending.clear();
        }
    }
    out
}

/// Parsed `Illustration_<w>x<h>@<scale>` metadata name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IllustrationInfo {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

impl IllustrationInfo {
    /// Parse a metadata key such as `Illustration_48x48@1` (attributes after `;` are
    /// ignored).
    pub fn parse(name: &str) -> Option<Self> {
        let rest = name.strip_prefix("Illustration_")?;
        let rest = rest.split(';').next()?;
        let (dims, scale) = rest.split_once('@')?;
        let (w, h) = dims.split_once('x')?;
        Some(Self {
            width: w.parse().ok()?,
            height: h.parse().ok()?,
            scale: scale.parse().ok()?,
        })
    }

    pub fn metadata_name(&self) -> String {
        format!("Illustration_{}x{}@{}", self.width, self.height, self.scale)
    }
}

/// Split the `Tags` metadata into plain tags and `_name:value` system tags.
pub fn parse_tags(value: &str) -> (Vec<String>, Vec<(String, String)>) {
    let mut plain = Vec::new();
    let mut system = Vec::new();
    for tag in value.split(';').map(str::trim).filter(|t| !t.is_empty()) {
        if let Some(rest) = tag.strip_prefix('_') {
            let (k, v) = rest.split_once(':').unwrap_or((rest, "yes"));
            system.push((k.to_string(), v.to_string()));
        } else {
            plain.push(tag.to_string());
        }
    }
    (plain, system)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter() {
        assert_eq!(
            parse_counter("image/jpeg=5;image/gif=3"),
            vec![("image/jpeg".into(), 5), ("image/gif".into(), 3)]
        );
        assert_eq!(
            parse_counter("text/html;raw=true=5;text/plain=1"),
            vec![("text/html;raw=true".into(), 5), ("text/plain".into(), 1)]
        );
        assert!(parse_counter("").is_empty());
    }

    #[test]
    fn illustration() {
        let i = IllustrationInfo::parse("Illustration_48x48@1").unwrap();
        assert_eq!((i.width, i.height, i.scale), (48, 48, 1.0));
        assert_eq!(i.metadata_name(), "Illustration_48x48@1");
        assert!(IllustrationInfo::parse("Illustration_48x48@2;name=foo").is_some());
        assert!(IllustrationInfo::parse("Title").is_none());
    }

    #[test]
    fn tags() {
        let (plain, sys) = parse_tags("wikipedia;_category:wikipedia;_ftindex:yes;_ftindex");
        assert_eq!(plain, vec!["wikipedia"]);
        assert_eq!(sys[0], ("category".into(), "wikipedia".into()));
        assert_eq!(sys[2], ("ftindex".into(), "yes".into()));
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;

    #[test]
    fn counter_edge_cases() {
        assert_eq!(parse_counter("text/html=5;"), vec![("text/html".into(), 5)]);
        assert_eq!(
            parse_counter(";;text/html=5"),
            vec![("text/html".into(), 5)],
            "empty leading chunks are dropped"
        );
        assert!(parse_counter("text/html").is_empty());
        assert!(parse_counter("=5").is_empty(), "empty mimetype");
        assert!(parse_counter("text/html=").is_empty(), "empty count");
        assert!(
            parse_counter("text/html=abc").is_empty(),
            "non-numeric count is dropped"
        );
        assert!(
            parse_counter("text/html=99999999999999999999999").is_empty(),
            "overflowing count is dropped"
        );
        assert_eq!(parse_counter("a/b=1;c/d=2;e/f=3").len(), 3);
    }

    #[test]
    fn illustration_edge_cases() {
        let i = IllustrationInfo::parse("Illustration_96x96@1.5").unwrap();
        assert_eq!((i.width, i.height, i.scale), (96, 96, 1.5));
        assert_eq!(i.metadata_name(), "Illustration_96x96@1.5");
        assert!(IllustrationInfo::parse("Illustration_48x48").is_none());
        assert!(IllustrationInfo::parse("Illustration_48@1").is_none());
        assert!(IllustrationInfo::parse("Illustration_axb@1").is_none());
        assert!(IllustrationInfo::parse("").is_none());
    }

    #[test]
    fn tags_edge_cases() {
        let (plain, sys) = parse_tags(" a ; b;;_x: y ;_z");
        assert_eq!(plain, vec!["a", "b"]);
        assert_eq!(
            sys,
            vec![
                ("x".to_string(), " y".to_string()),
                ("z".to_string(), "yes".to_string())
            ]
        );
        assert_eq!(parse_tags(""), (vec![], vec![]));
    }
}
