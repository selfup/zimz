// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Structural checks modelled on libzim's `IntegrityCheck`.

use std::cmp::Ordering;

use crate::Archive;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Check {
    /// MD5 over the whole archive (reads every byte).
    Checksum,
    /// Every dirent pointer lies inside the archive.
    DirentPointers,
    /// Dirents parse and are strictly ordered by `(namespace, path)`.
    DirentOrder,
    /// Title-ordered list entries are in range and ordered by `(namespace, title)`.
    TitleIndex,
    /// Every cluster pointer lies inside the archive.
    ClusterPointers,
    /// Item MIME indexes point into the MIME list.
    DirentMimeTypes,
    /// Every cluster decodes and has a valid offset table (decompresses everything).
    Clusters,
}

impl Check {
    pub const ALL: [Check; 7] = [
        Check::Checksum,
        Check::DirentPointers,
        Check::DirentOrder,
        Check::TitleIndex,
        Check::ClusterPointers,
        Check::DirentMimeTypes,
        Check::Clusters,
    ];
    /// Everything that does not read the whole archive.
    pub const QUICK: [Check; 5] = [
        Check::DirentPointers,
        Check::DirentOrder,
        Check::TitleIndex,
        Check::ClusterPointers,
        Check::DirentMimeTypes,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub check: Check,
    pub message: String,
}

/// Run the given checks; returns every problem found (an empty list means all passed).
pub fn run(archive: &Archive, checks: &[Check]) -> Vec<Problem> {
    checks
        .iter()
        .filter_map(|&check| {
            let message = match check {
                Check::Checksum => checksum(archive),
                Check::DirentPointers => dirent_pointers(archive),
                Check::DirentOrder => dirent_order(archive),
                Check::TitleIndex => title_index(archive),
                Check::ClusterPointers => cluster_pointers(archive),
                Check::DirentMimeTypes => dirent_mime_types(archive),
                Check::Clusters => clusters(archive),
            };
            message.map(|message| Problem { check, message })
        })
        .collect()
}

fn checksum(archive: &Archive) -> Option<String> {
    match archive.verify_checksum() {
        Ok(true) => None,
        Ok(false) => Some("MD5 checksum does not match".into()),
        Err(e) => Some(format!("cannot verify checksum: {e}")),
    }
}

fn dirent_pointers(archive: &Archive) -> Option<String> {
    (0..archive.entry_count()).find_map(|i| archive.dirent_offset(i).err().map(|e| e.to_string()))
}

fn dirent_order(archive: &Archive) -> Option<String> {
    let mut prev: Option<(u8, String)> = None;
    for (i, d) in archive.entries().enumerate() {
        let d = match d {
            Ok(d) => d,
            Err(e) => return Some(format!("entry {i}: {e}")),
        };
        let key = (d.namespace, d.path.clone());
        if let Some(p) = &prev
            && p.cmp(&key) != Ordering::Less
        {
            return Some(format!(
                "entries {} and {i} are not in ascending path order",
                i - 1
            ));
        }
        prev = Some(key);
    }
    None
}

fn title_index(archive: &Archive) -> Option<String> {
    let mut prev: Option<(u8, String)> = None;
    for pos in 0..archive.title_index().len() {
        let d = match archive.entry_by_title_position(pos) {
            Ok(d) => d,
            Err(e) => return Some(format!("title position {pos}: {e}")),
        };
        let key = (d.namespace, d.title().to_string());
        if let Some(p) = &prev
            && p.cmp(&key) == Ordering::Greater
        {
            return Some(format!(
                "title positions {} and {pos} are not in title order",
                pos - 1
            ));
        }
        prev = Some(key);
    }
    None
}

fn cluster_pointers(archive: &Archive) -> Option<String> {
    (0..archive.cluster_count())
        .find_map(|i| archive.cluster_offset(i).err().map(|e| e.to_string()))
}

fn dirent_mime_types(archive: &Archive) -> Option<String> {
    let n = archive.mime_list().len();
    for (i, d) in archive.entries().enumerate() {
        match d {
            Err(e) => return Some(format!("entry {i}: {e}")),
            Ok(d) if d.is_item() && usize::from(d.mime) >= n => {
                return Some(format!(
                    "entry {i} ({}) has MIME index {} but the list has {n} types",
                    d.full_path(),
                    d.mime
                ));
            }
            Ok(_) => {}
        }
    }
    None
}

fn clusters(archive: &Archive) -> Option<String> {
    (0..archive.cluster_count()).find_map(|i| {
        archive
            .cluster(i)
            .err()
            .map(|e| format!("cluster {i}: {e}"))
    })
}
