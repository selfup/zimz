// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Property tests: parsers never panic on arbitrary bytes, and the builder → reader
//! round trip preserves every entry.

mod common;

use std::collections::BTreeSet;

use common::builder::ZimBuilder;
use common::open_bytes;
use proptest::prelude::*;
use zimz_core::integrity::{self, Check};
use zimz_core::metadata::parse_counter;
use zimz_core::mime::MIME_REDIRECT;
use zimz_core::{Dirent, DirentKind, Header, MimeList};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn header_parse_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
        if let Ok(h) = Header::parse(&bytes) {
            let _ = h.validate(bytes.len() as u64);
            let _ = h.uuid_string();
        }
    }

    #[test]
    fn dirent_parse_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..300)) {
        if let Ok(Some((d, used))) = Dirent::parse(7, &bytes) {
            prop_assert!(used <= bytes.len());
            prop_assert_eq!(d.index, 7);
            let _ = d.title();
            let _ = d.full_path();
        }
    }

    #[test]
    fn mime_parse_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..300)) {
        if let Ok(m) = MimeList::parse(&bytes) {
            prop_assert!(m.len() <= bytes.len());
        }
    }

    #[test]
    fn random_bytes_as_archive_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..1500)) {
        if let Ok(a) = open_bytes(bytes) {
            let _ = a.main_entry();
            for e in a.entries().take(50).flatten() {
                let _ = a.item_data(&e);
            }
            let _ = integrity::run(&a, &Check::QUICK);
        }
    }

    #[test]
    fn dirent_roundtrip(
        ns in any::<u8>(),
        path in "[^\x00]{0,300}",
        title in "[^\x00]{0,100}",
        cluster in any::<u32>(),
        blob in any::<u32>(),
        params in prop::collection::vec(any::<u8>(), 0..5),
        redirect in any::<bool>(),
    ) {
        let mut b = Vec::new();
        b.extend_from_slice(&(if redirect { MIME_REDIRECT } else { 3u16 }).to_le_bytes());
        b.push(params.len() as u8);
        b.push(ns);
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&cluster.to_le_bytes());
        if !redirect {
            b.extend_from_slice(&blob.to_le_bytes());
        }
        b.extend_from_slice(path.as_bytes());
        b.push(0);
        b.extend_from_slice(title.as_bytes());
        b.push(0);
        b.extend_from_slice(&params);
        b.extend_from_slice(b"trailing");
        let (d, used) = Dirent::parse(1, &b).unwrap().unwrap();
        prop_assert_eq!(used, b.len() - 8);
        prop_assert_eq!(&d.path, &path);
        prop_assert_eq!(&d.title, &title);
        prop_assert_eq!(d.namespace, ns);
        if redirect {
            prop_assert_eq!(d.kind, DirentKind::Redirect { target: cluster });
        } else {
            prop_assert_eq!(d.kind, DirentKind::Item { cluster, blob });
        }
        // any strict prefix is "need more"
        prop_assert!(Dirent::parse(1, &b[..b.len() - 9]).unwrap().is_none() || params.is_empty());
    }

    #[test]
    fn counter_roundtrip(pairs in prop::collection::vec(("[a-z]{1,8}/[a-z0-9.+-]{1,12}", 0u64..10_000_000), 0..8)) {
        let text: Vec<String> = pairs.iter().map(|(m, n)| format!("{m}={n}")).collect();
        let parsed = parse_counter(&text.join(";"));
        prop_assert_eq!(parsed, pairs);
    }

    #[test]
    fn builder_reader_roundtrip(
        entries in prop::collection::vec(
            ("[a-zA-Z0-9_ .-]{1,24}", "[^\x00]{0,24}", prop::collection::vec(any::<u8>(), 0..300), any::<bool>()),
            1..20,
        ),
        new_scheme in any::<bool>(),
    ) {
        let mut b = if new_scheme { ZimBuilder::new_scheme() } else { ZimBuilder::old_scheme() };
        let ns = b.content_ns();
        let mut seen = BTreeSet::new();
        let mut expected = Vec::new();
        for (path, title, data, front) in &entries {
            if !seen.insert(path.clone()) {
                continue;
            }
            b = b.item(ns, path, title, "text/plain", data.clone()).front(*front);
            expected.push((path.clone(), title.clone(), data.clone(), *front));
        }
        let a = open_bytes(b.build()).unwrap();
        prop_assert!(integrity::run(&a, &Check::ALL).is_empty());
        for (path, title, data, _) in &expected {
            let e = a.entry_by_path(ns, path).unwrap().unwrap();
            let want_title = if title.is_empty() { path } else { title };
            prop_assert_eq!(e.title(), want_title.as_str());
            prop_assert_eq!(&*a.item_data(&e).unwrap(), &data[..]);
        }
        let fronts = expected.iter().filter(|e| e.3).count() as u32;
        if new_scheme {
            prop_assert_eq!(a.title_index().len(), fronts);
        } else {
            prop_assert_eq!(a.title_index().len(), a.entry_count());
        }
        let mut prev: Option<(u8, String)> = None;
        for e in a.title_ordered() {
            let e = e.unwrap();
            let key = (e.namespace, e.title().to_string());
            if let Some(p) = &prev {
                prop_assert!(p <= &key);
            }
            prev = Some(key);
        }
    }
}
