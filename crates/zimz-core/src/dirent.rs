// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Directory entries.

use crate::mime::{MIME_DELETED, MIME_LINKTARGET, MIME_REDIRECT};
use crate::{Error, Result};

/// Maximum redirect hops followed before giving up (libzim uses the same value).
pub const REDIRECT_HOP_LIMIT: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DirentKind {
    Item { cluster: u32, blob: u32 },
    Redirect { target: u32 },
    LinkTarget,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirent {
    /// Position in the path pointer list.
    pub index: u32,
    pub namespace: u8,
    /// Index into the MIME list, or one of the special values for non-items.
    pub mime: u16,
    pub path: String,
    /// Raw stored title; empty means "same as path" (see [`Dirent::title`]).
    pub title: String,
    pub kind: DirentKind,
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl Dirent {
    /// Parse a dirent at the start of `bytes`. Returns `Ok(None)` when `bytes` is too
    /// short to contain the whole entry (the caller should retry with more bytes).
    pub fn parse(index: u32, bytes: &[u8]) -> Result<Option<(Self, usize)>> {
        if bytes.len() < 12 {
            return Ok(None);
        }
        let mime = u16::from_le_bytes([bytes[0], bytes[1]]);
        let param_len = usize::from(bytes[2]);
        let namespace = bytes[3];
        let (kind, mut pos) = match mime {
            MIME_REDIRECT => (
                DirentKind::Redirect {
                    target: u32_at(bytes, 8),
                },
                12,
            ),
            MIME_LINKTARGET | MIME_DELETED => {
                if bytes.len() < 16 {
                    return Ok(None);
                }
                (
                    if mime == MIME_LINKTARGET {
                        DirentKind::LinkTarget
                    } else {
                        DirentKind::Deleted
                    },
                    16,
                )
            }
            _ => {
                if bytes.len() < 16 {
                    return Ok(None);
                }
                (
                    DirentKind::Item {
                        cluster: u32_at(bytes, 8),
                        blob: u32_at(bytes, 12),
                    },
                    16,
                )
            }
        };
        let Some(path_end) = bytes[pos..].iter().position(|&b| b == 0) else {
            return Ok(None);
        };
        let path = String::from_utf8_lossy(&bytes[pos..pos + path_end]).into_owned();
        pos += path_end + 1;
        let Some(title_end) = bytes[pos..].iter().position(|&b| b == 0) else {
            return Ok(None);
        };
        let title = String::from_utf8_lossy(&bytes[pos..pos + title_end]).into_owned();
        pos += title_end + 1;
        if bytes.len() < pos + param_len {
            return Ok(None);
        }
        pos += param_len;
        Ok(Some((
            Self {
                index,
                namespace,
                mime,
                path,
                title,
                kind,
            },
            pos,
        )))
    }

    /// The display title: the stored title, or the path when the title is empty.
    pub fn title(&self) -> &str {
        if self.title.is_empty() {
            &self.path
        } else {
            &self.title
        }
    }

    /// `<namespace>/<path>`.
    pub fn full_path(&self) -> String {
        format!("{}/{}", self.namespace as char, self.path)
    }

    pub fn is_redirect(&self) -> bool {
        matches!(self.kind, DirentKind::Redirect { .. })
    }

    pub fn is_item(&self) -> bool {
        matches!(self.kind, DirentKind::Item { .. })
    }

    pub fn redirect_target(&self) -> Option<u32> {
        match self.kind {
            DirentKind::Redirect { target } => Some(target),
            _ => None,
        }
    }

    /// `(cluster, blob)` for items.
    pub fn location(&self) -> Option<(u32, u32)> {
        match self.kind {
            DirentKind::Item { cluster, blob } => Some((cluster, blob)),
            _ => None,
        }
    }

    pub(crate) fn location_or_err(&self) -> Result<(u32, u32)> {
        self.location().ok_or(Error::NotAnItem(self.index))
    }
}

/// Split `"C/foo/bar"` into `(b'C', "foo/bar")`. A leading `/` is tolerated.
pub fn split_long_path(long: &str) -> Option<(u8, &str)> {
    let s = long.strip_prefix('/').unwrap_or(long);
    let mut it = s.as_bytes().iter();
    let ns = *it.next()?;
    if ns == b'/' || it.next() != Some(&b'/') {
        return None;
    }
    Some((ns, &s[2..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_item_and_redirect() {
        let mut b = vec![0u8; 16];
        b[0..2].copy_from_slice(&3u16.to_le_bytes());
        b[3] = b'C';
        b[8..12].copy_from_slice(&7u32.to_le_bytes());
        b[12..16].copy_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(b"Some_Path\0Some title\0trailing");
        let (d, used) = Dirent::parse(42, &b).unwrap().unwrap();
        assert_eq!(used, 16 + 10 + 11);
        assert_eq!(d.namespace, b'C');
        assert_eq!(
            d.kind,
            DirentKind::Item {
                cluster: 7,
                blob: 2
            }
        );
        assert_eq!(d.path, "Some_Path");
        assert_eq!(d.title(), "Some title");
        assert_eq!(d.full_path(), "C/Some_Path");

        let mut r = vec![0u8; 12];
        r[0..2].copy_from_slice(&MIME_REDIRECT.to_le_bytes());
        r[3] = b'W';
        r[8..12].copy_from_slice(&99u32.to_le_bytes());
        r.extend_from_slice(b"mainPage\0\0");
        let (d, _) = Dirent::parse(0, &r).unwrap().unwrap();
        assert_eq!(d.redirect_target(), Some(99));
        assert_eq!(d.title(), "mainPage", "empty title falls back to path");
    }

    #[test]
    fn needs_more_bytes() {
        assert!(Dirent::parse(0, &[0u8; 5]).unwrap().is_none());
        let mut b = vec![0u8; 16];
        b.extend_from_slice(b"unterminated");
        assert!(Dirent::parse(0, &b).unwrap().is_none());
    }

    #[test]
    fn long_path_split() {
        assert_eq!(split_long_path("C/a/b"), Some((b'C', "a/b")));
        assert_eq!(split_long_path("/A/x"), Some((b'A', "x")));
        assert_eq!(split_long_path("Cx"), None);
        assert_eq!(split_long_path(""), None);
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;

    fn fixed(mime: u16, ns: u8, param_len: u8) -> Vec<u8> {
        let mut b = vec![0u8; 16];
        b[0..2].copy_from_slice(&mime.to_le_bytes());
        b[2] = param_len;
        b[3] = ns;
        b
    }

    #[test]
    fn deprecated_kinds_parse_with_the_item_layout() {
        for (mime, kind) in [
            (MIME_LINKTARGET, DirentKind::LinkTarget),
            (MIME_DELETED, DirentKind::Deleted),
        ] {
            let mut b = fixed(mime, b'A', 0);
            b.extend_from_slice(b"p\0t\0");
            let (d, used) = Dirent::parse(0, &b).unwrap().unwrap();
            assert_eq!(d.kind, kind);
            assert_eq!(used, 20);
            assert!(!d.is_item() && !d.is_redirect());
            assert_eq!(d.location(), None);
            assert!(matches!(d.location_or_err(), Err(Error::NotAnItem(0))));
            assert!(
                Dirent::parse(0, &b[..14]).unwrap().is_none(),
                "needs 16 fixed bytes"
            );
        }
    }

    #[test]
    fn parameters_are_skipped_and_required() {
        let mut b = fixed(1, b'C', 3);
        b.extend_from_slice(b"p\0t\0");
        assert!(
            Dirent::parse(0, &b).unwrap().is_none(),
            "3 parameter bytes missing"
        );
        b.extend_from_slice(&[9, 9, 9]);
        let (d, used) = Dirent::parse(0, &b).unwrap().unwrap();
        assert_eq!(used, b.len());
        assert_eq!(d.path, "p");
    }

    #[test]
    fn invalid_utf8_strings_are_lossy() {
        let mut b = fixed(1, b'C', 0);
        b.extend_from_slice(b"pa\xffth\0ti\xfele\0");
        let (d, _) = Dirent::parse(0, &b).unwrap().unwrap();
        assert!(d.path.contains('\u{fffd}') && d.title.contains('\u{fffd}'));
    }

    #[test]
    fn accessors() {
        let mut b = fixed(4, b'C', 0);
        b[8..12].copy_from_slice(&5u32.to_le_bytes());
        b[12..16].copy_from_slice(&6u32.to_le_bytes());
        b.extend_from_slice(b"path\0\0");
        let (d, _) = Dirent::parse(9, &b).unwrap().unwrap();
        assert_eq!(d.location(), Some((5, 6)));
        assert_eq!(d.location_or_err().unwrap(), (5, 6));
        assert_eq!(d.redirect_target(), None);
        assert_eq!(d.full_path(), "C/path");
        assert_eq!(d.title(), "path");
        assert_eq!(d.index, 9);
    }

    #[test]
    fn very_long_strings() {
        let mut b = fixed(1, b'C', 0);
        let path = "x".repeat(70_000);
        b.extend_from_slice(path.as_bytes());
        b.push(0);
        b.push(0);
        let (d, used) = Dirent::parse(0, &b).unwrap().unwrap();
        assert_eq!(used, 16 + 70_001 + 1);
        assert_eq!(d.path.len(), 70_000);
    }
}
