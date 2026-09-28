// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Title-ordered listings.

/// Where the title-ordered entry list comes from (libzim's own preference order:
/// the `X/listing/titleOrdered/v1` blob, then the header's title pointer list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleIndex {
    None,
    /// The header's title pointer list: every entry, sorted by `(namespace, title)`.
    Header {
        offset: u64,
        count: u32,
    },
    /// `X/listing/titleOrdered/v1`: front articles only, sorted by title.
    FrontArticles {
        offset: u64,
        count: u32,
    },
}

impl TitleIndex {
    pub fn len(&self) -> u32 {
        match self {
            Self::None => 0,
            Self::Header { count, .. } | Self::FrontArticles { count, .. } => *count,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_front_articles_only(&self) -> bool {
        matches!(self, Self::FrontArticles { .. })
    }

    pub(crate) fn offset(&self) -> Option<u64> {
        match self {
            Self::None => None,
            Self::Header { offset, .. } | Self::FrontArticles { offset, .. } => Some(*offset),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessors() {
        assert_eq!(TitleIndex::None.len(), 0);
        assert!(TitleIndex::None.is_empty());
        assert_eq!(TitleIndex::None.offset(), None);
        let h = TitleIndex::Header {
            offset: 10,
            count: 3,
        };
        assert_eq!(
            (
                h.len(),
                h.is_empty(),
                h.is_front_articles_only(),
                h.offset()
            ),
            (3, false, false, Some(10))
        );
        let f = TitleIndex::FrontArticles {
            offset: 20,
            count: 0,
        };
        assert_eq!(
            (
                f.len(),
                f.is_empty(),
                f.is_front_articles_only(),
                f.offset()
            ),
            (0, true, true, Some(20))
        );
    }
}
