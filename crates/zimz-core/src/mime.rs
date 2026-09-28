// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! The MIME type list that follows the header.

use crate::{Error, Result};

/// Dirent `mimetype` value of a redirect.
pub const MIME_REDIRECT: u16 = 0xffff;
/// Dirent `mimetype` value of a (deprecated) link target.
pub const MIME_LINKTARGET: u16 = 0xfffe;
/// Dirent `mimetype` value of a (deprecated) deleted entry.
pub const MIME_DELETED: u16 = 0xfffd;

/// Marker MIME type of the embedded Xapian databases.
pub const MIME_XAPIAN: &str = "application/octet-stream+xapian";
/// Marker MIME type of title listings.
pub const MIME_LISTING: &str = "application/octet-stream+zimlisting";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MimeList {
    types: Vec<String>,
}

impl MimeList {
    /// Parse NUL-terminated strings up to the empty terminator string.
    pub fn parse(window: &[u8]) -> Result<Self> {
        let mut types = Vec::new();
        let mut pos = 0;
        loop {
            let Some(rel) = window[pos..].iter().position(|&b| b == 0) else {
                return Err(Error::corrupt("MIME type list is not terminated"));
            };
            if rel == 0 {
                break;
            }
            types.push(String::from_utf8_lossy(&window[pos..pos + rel]).into_owned());
            pos += rel + 1;
        }
        Ok(Self { types })
    }

    pub fn get(&self, index: u16) -> Option<&str> {
        self.types.get(usize::from(index)).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.types.iter().map(String::as_str)
    }

    /// Redirect / linktarget / deleted markers, not indexes into the list.
    pub fn is_special(index: u16) -> bool {
        index >= MIME_DELETED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list() {
        let m = MimeList::parse(b"text/html\0image/png\0\0garbage").unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.get(1), Some("image/png"));
        assert_eq!(m.get(2), None);
        assert!(MimeList::parse(b"text/html\0image/png").is_err());
        assert!(MimeList::parse(b"\0").unwrap().is_empty());
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;

    #[test]
    fn special_indexes() {
        assert!(MimeList::is_special(MIME_REDIRECT));
        assert!(MimeList::is_special(MIME_LINKTARGET));
        assert!(MimeList::is_special(MIME_DELETED));
        assert!(!MimeList::is_special(0xfffc));
        assert!(!MimeList::is_special(0));
    }

    #[test]
    fn invalid_utf8_is_lossy_not_fatal() {
        let m = MimeList::parse(b"text/\xff\xfeplain\0\0").unwrap();
        assert_eq!(m.len(), 1);
        assert!(m.get(0).unwrap().starts_with("text/"));
        assert!(m.get(0).unwrap().contains('\u{fffd}'));
    }

    #[test]
    fn iteration_order_and_bounds() {
        let m = MimeList::parse(b"a\0b\0c\0\0").unwrap();
        assert_eq!(m.iter().collect::<Vec<_>>(), vec!["a", "b", "c"]);
        assert_eq!(m.get(3), None);
        assert_eq!(m.get(u16::MAX), None);
        assert!(MimeList::parse(b"").is_err());
        assert!(MimeList::parse(b"abc").is_err());
    }
}
