// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Title suggestions over the title index (`X/title/xapian`), reproducing libzim's
//! `SuggestionSearcher` (`src/suggestion.cpp`):
//!
//! ```text
//! xquery   = AND(Z-stems of all words, last word as OR(SYNONYM(prefix*), Z-stem))
//! phrase   = PHRASE(unstemmed words, window = n)
//! anchored = PHRASE("0posanchor", unstemmed words, window = n + 1)
//! query    = OR(xquery, phrase, anchored)
//! ```
//!
//! weighted with `BM25(k1 = 0.001, b = 1)`, sorted by weight then title, and collapsed on
//! the redirect target (value slot `targetPath`).

use std::collections::{BinaryHeap, HashSet};

use unicode_general_category::{GeneralCategory as Cat, get_general_category};

use crate::analyzer::{Analyzer, is_wordchar};
use crate::db::GlassDb;
use crate::position::{decode_positions, make_key as position_key};
use crate::postlist::PostList;
use crate::version::TableKind;
use crate::{Error, Result};

/// libzim's `ANCHOR_TERM` (without the trailing space).
pub const ANCHOR_TERM: &str = "0posanchor";
/// Xapian's default `max_partial_expansion` with `WILDCARD_LIMIT_MOST_FREQUENT`.
pub const MAX_PARTIAL_EXPANSION: usize = 100;

const K1: f64 = 0.001;
const K3: f64 = 1.0;
const B: f64 = 1.0;
const MIN_NORMLEN: f64 = 0.5;

/// BM25 term weight with libzim's suggestion parameters.
fn term_weight(n: u32, tf: u32) -> f64 {
    let mut tw = (f64::from(n) - f64::from(tf) + 0.5) / (f64::from(tf) + 0.5);
    if tw < 2.0 {
        tw = tw * 0.5 + 1.0;
    }
    // wqf = 1: (k3 + 1) * 1 / (k3 + 1) = 1
    let _ = K3;
    tw.ln() * (K1 + 1.0)
}

fn doc_part(term_weight: f64, wdf: u32, doc_len: u32, len_factor: f64) -> f64 {
    let normlen = (f64::from(doc_len) * len_factor).max(MIN_NORMLEN);
    let wdf = f64::from(wdf);
    term_weight * (wdf / (K1 * (normlen * B + (1.0 - B)) + wdf))
}

/// Xapian's `should_stem`: only terms starting with a letter are stemmed.
fn should_stem(term: &str) -> bool {
    term.chars().next().is_some_and(|c| {
        matches!(
            get_general_category(c),
            Cat::LowercaseLetter | Cat::TitlecaseLetter | Cat::ModifierLetter | Cat::OtherLetter
        )
    })
}

/// `Xapian::QueryParser::STEM_SOME` term for a query word.
fn stem_some(analyzer: &Analyzer, word: &str) -> String {
    if analyzer.algorithm().is_some() && should_stem(word) {
        format!("Z{}", analyzer.stem(word))
    } else {
        word.to_string()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub docid: u32,
    pub weight: f64,
    /// Original title (value slot `title`).
    pub title: String,
    /// Document data, i.e. the entry's full path (`C/…`).
    pub path: String,
    /// Redirect target (value slot `targetPath`), when the index stores one.
    pub target_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SuggestionResults {
    pub hits: Vec<Suggestion>,
    /// Matching documents before collapsing redirects onto their targets.
    pub total: u32,
}

/// Merged posting stream over the expansions of a wildcard, weighted as one synonym.
struct Union<'a> {
    lists: Vec<PostList<'a>>,
    heap: BinaryHeap<std::cmp::Reverse<(u32, usize)>>,
    /// Lists positioned on the current docid (taken out of the heap until advanced).
    members: Vec<usize>,
    docid: u32,
    wdf: u32,
    at_end: bool,
}

impl<'a> Union<'a> {
    fn new(lists: Vec<PostList<'a>>) -> Self {
        let mut u = Self {
            heap: BinaryHeap::new(),
            lists,
            members: Vec::new(),
            docid: 0,
            wdf: 0,
            at_end: true,
        };
        for (i, l) in u.lists.iter().enumerate() {
            if !l.at_end() {
                u.heap.push(std::cmp::Reverse((l.docid(), i)));
            }
        }
        u.settle();
        u
    }

    /// Position on the smallest current docid, summing the wdf of every list at it.
    fn settle(&mut self) {
        let Some(std::cmp::Reverse((did, _))) = self.heap.peek().copied() else {
            self.at_end = true;
            return;
        };
        self.at_end = false;
        self.docid = did;
        self.wdf = 0;
        let mut members = Vec::new();
        while let Some(std::cmp::Reverse((d, i))) = self.heap.peek().copied() {
            if d != did {
                break;
            }
            self.heap.pop();
            self.wdf = self.wdf.saturating_add(self.lists[i].wdf());
            members.push(i);
        }
        // keep the members out of the heap until advanced
        self.members = members;
    }
}

impl Union<'_> {
    fn skip_to(&mut self, target: u32) -> Result<bool> {
        if self.at_end {
            return Ok(false);
        }
        if self.docid >= target {
            return Ok(true);
        }
        for i in std::mem::take(&mut self.members) {
            if self.lists[i].skip_to(target)? {
                self.heap
                    .push(std::cmp::Reverse((self.lists[i].docid(), i)));
            }
        }
        // lists still in the heap may also be below the target
        let mut rebuilt = BinaryHeap::new();
        for std::cmp::Reverse((d, i)) in self.heap.drain() {
            if d >= target {
                rebuilt.push(std::cmp::Reverse((d, i)));
            } else if self.lists[i].skip_to(target)? {
                rebuilt.push(std::cmp::Reverse((self.lists[i].docid(), i)));
            }
        }
        self.heap = rebuilt;
        self.settle();
        Ok(!self.at_end)
    }
}

/// Xapian's OR-tree term-frequency estimate: repeatedly merge the two smallest with
/// `l + r - l·r/N`.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]
fn synonym_termfreq_estimate(freqs: &[u32], n: u32) -> u32 {
    if freqs.is_empty() {
        return 0;
    }
    let mut heap: BinaryHeap<std::cmp::Reverse<u64>> = freqs
        .iter()
        .map(|&f| std::cmp::Reverse(u64::from(f)))
        .collect();
    while heap.len() > 1 {
        let std::cmp::Reverse(r) = heap.pop().unwrap();
        let std::cmp::Reverse(l) = heap.pop().unwrap();
        let est = l as f64 + r as f64 - (l as f64 * r as f64 / f64::from(n));
        heap.push(std::cmp::Reverse((est + 0.5) as u64));
    }
    heap.pop().map_or(0, |std::cmp::Reverse(v)| {
        u32::try_from(v).unwrap_or(u32::MAX)
    })
}

/// `PhrasePostList::test_doc` over already-decoded position lists.
fn phrase_matches(poslists: &[Vec<u32>], window: u32) -> bool {
    let n = poslists.len();
    if n == 0 || poslists[0].is_empty() {
        return false;
    }
    let mut idx0 = 0usize;
    loop {
        let base = poslists[0][idx0];
        let mut pos = base;
        let mut i = 0usize;
        let b;
        loop {
            i += 1;
            if i == n {
                return true;
            }
            let Some(&p) = poslists[i].iter().find(|&&p| p > pos) else {
                return false;
            };
            pos = p;
            let projected = pos + (n - i) as u32;
            if projected - base > window {
                b = projected;
                break;
            }
        }
        let target = b - window;
        match poslists[0].iter().position(|&p| p >= target) {
            Some(j) if j > idx0 => idx0 = j,
            Some(j) => idx0 = j + 1,
            None => return false,
        }
        if idx0 >= poslists[0].len() {
            return false;
        }
    }
}

fn positions(db: &GlassDb<'_>, did: u32, term: &str) -> Result<Option<Vec<u32>>> {
    let key = position_key(term.as_bytes(), did);
    match db.cursor(TableKind::Position).get_exact(&key)? {
        Some(tag) => Ok(Some(decode_positions(&tag)?)),
        None => Ok(None),
    }
}

struct Scored {
    docid: u32,
    weight: f64,
}

/// libzim-compatible title suggestions for `text`.
#[allow(clippy::too_many_lines)]
pub fn suggest(
    db: &GlassDb<'_>,
    analyzer: &Analyzer,
    text: &str,
    offset: usize,
    limit: usize,
) -> Result<SuggestionResults> {
    let n_docs = db.doc_count();
    let avg = db.avg_doclen();
    let len_factor = if avg > 0.0 { 1.0 / avg } else { 0.0 };
    let query = Analyzer::remove_accents(text);
    let words = analyzer.tokenize(&query);
    if words.is_empty() {
        return Ok(SuggestionResults::default());
    }
    let partial = query.chars().last().is_some_and(is_wordchar);

    // --- xquery: AND of fixed stems + partial branch
    let (fixed_words, partial_word) = if partial {
        (&words[..words.len() - 1], Some(&words[words.len() - 1]))
    } else {
        (&words[..], None)
    };
    let mut fixed: Vec<(PostList<'_>, f64)> = Vec::new();
    for w in fixed_words {
        let term = stem_some(analyzer, w);
        let Some(pl) = db.postlist(&term)? else {
            return Ok(SuggestionResults::default());
        };
        let weight = term_weight(n_docs, pl.term_freq());
        fixed.push((pl, weight));
    }
    fixed.sort_by_key(|(pl, _)| pl.term_freq());

    // partial branch: the most frequent expansions as one synonym, OR the stem itself
    let mut synonym: Option<(Union<'_>, f64)> = None;
    let mut alt: Option<(PostList<'_>, f64)> = None;
    if let Some(p) = partial_word {
        let mut expansions: Vec<_> = db.terms(p.as_bytes()).collect::<Result<Vec<_>>>()?;
        if expansions.len() > MAX_PARTIAL_EXPANSION {
            expansions.sort_by(|a, b| b.term_freq.cmp(&a.term_freq).then(a.term.cmp(&b.term)));
            expansions.truncate(MAX_PARTIAL_EXPANSION);
        }
        if !expansions.is_empty() {
            let freqs: Vec<u32> = expansions.iter().map(|e| e.term_freq).collect();
            let est = synonym_termfreq_estimate(&freqs, n_docs);
            let lists = expansions
                .iter()
                .filter_map(|e| db.postlist_bytes(&e.term).transpose())
                .collect::<Result<Vec<_>>>()?;
            synonym = Some((Union::new(lists), term_weight(n_docs, est)));
        }
        let stem = stem_some(analyzer, p);
        if let Some(pl) = db.postlist(&stem)? {
            let weight = term_weight(n_docs, pl.term_freq());
            alt = Some((pl, weight));
        }
        if synonym.is_none() && alt.is_none() {
            return Ok(SuggestionResults::default());
        }
    }

    // --- phrase subqueries use the unstemmed words (STEM_NONE)
    let mut phrase_terms: Vec<(String, f64)> = Vec::new();
    let mut phrase_possible = true;
    for w in &words {
        match db.postlist(w)? {
            Some(pl) => phrase_terms.push((w.clone(), term_weight(n_docs, pl.term_freq()))),
            None => phrase_possible = false,
        }
    }
    let anchor_weight = db
        .postlist(ANCHOR_TERM)?
        .map(|pl| term_weight(n_docs, pl.term_freq()));
    let mut phrase_lists: Vec<PostList<'_>> = Vec::new();
    if phrase_possible {
        for (w, _) in &phrase_terms {
            phrase_lists.push(
                db.postlist(w)?
                    .ok_or_else(|| Error::corrupt("term vanished"))?,
            );
        }
    }

    let mut doclens = db
        .doc_lengths()?
        .ok_or_else(|| Error::corrupt("title index has no document lengths"))?;
    let mut matches: Vec<Scored> = Vec::new();

    // candidate stream: intersection of fixed terms and the partial branch (if any)
    let mut cand: Option<u32> = None;
    let has_fixed = !fixed.is_empty();
    let next_candidate = |fixed: &mut Vec<(PostList<'_>, f64)>,
                          synonym: &mut Option<(Union<'_>, f64)>,
                          alt: &mut Option<(PostList<'_>, f64)>,
                          prev: Option<u32>|
     -> Result<Option<u32>> {
        let mut target = prev.map_or(1, |p| p + 1);
        'outer: loop {
            // smallest docid >= target common to all fixed lists
            if has_fixed {
                let mut i = 0;
                while i < fixed.len() {
                    if !fixed[i].0.skip_to(target)? {
                        return Ok(None);
                    }
                    let d = fixed[i].0.docid();
                    if d != target {
                        target = d;
                        i = 0;
                        continue;
                    }
                    i += 1;
                }
            }
            // partial branch: the doc must be in the synonym union or the alt list
            if synonym.is_some() || alt.is_some() {
                let mut branch_min: Option<u32> = None;
                if let Some((u, _)) = synonym.as_mut()
                    && u.skip_to(target)?
                {
                    branch_min = Some(u.docid);
                }
                if let Some((pl, _)) = alt.as_mut()
                    && pl.skip_to(target)?
                {
                    branch_min = Some(branch_min.map_or(pl.docid(), |m| m.min(pl.docid())));
                }
                match branch_min {
                    None => return Ok(None),
                    Some(d) if d == target => {}
                    Some(d) => {
                        target = d;
                        if has_fixed {
                            continue 'outer;
                        }
                    }
                }
            } else if !has_fixed {
                return Ok(None);
            }
            return Ok(Some(target));
        }
    };

    while let Some(did) = next_candidate(&mut fixed, &mut synonym, &mut alt, cand)? {
        cand = Some(did);
        let len = if doclens.jump_to(did)? {
            doclens.wdf()
        } else {
            0
        };
        let mut weight = 0.0;
        for (pl, w) in &fixed {
            weight += doc_part(*w, pl.wdf(), len, len_factor);
        }
        if let Some((u, w)) = &synonym
            && !u.at_end
            && u.docid == did
        {
            weight += doc_part(*w, u.wdf.min(len), len, len_factor);
        }
        if let Some((pl, w)) = &alt
            && !pl.at_end()
            && pl.docid() == did
        {
            weight += doc_part(*w, pl.wdf(), len, len_factor);
        }
        // phrase and anchored phrase: all unstemmed words present, in order
        if phrase_possible {
            let mut all = true;
            let mut wdfs = Vec::with_capacity(phrase_lists.len());
            for pl in &mut phrase_lists {
                if pl.skip_to(did)? && pl.docid() == did {
                    wdfs.push(pl.wdf());
                } else {
                    all = false;
                    break;
                }
            }
            if all {
                let mut poslists = Vec::with_capacity(words.len() + 1);
                let mut have_positions = true;
                for (w, _) in &phrase_terms {
                    match positions(db, did, w)? {
                        Some(p) if !p.is_empty() => poslists.push(p),
                        _ => {
                            have_positions = false;
                            break;
                        }
                    }
                }
                if have_positions {
                    let phrase_weight: f64 = phrase_terms
                        .iter()
                        .zip(&wdfs)
                        .map(|((_, w), &wdf)| doc_part(*w, wdf, len, len_factor))
                        .sum();
                    if phrase_matches(&poslists, words.len() as u32) {
                        weight += phrase_weight;
                    }
                    if let Some(aw) = anchor_weight
                        && let Some(anchor_pos) = positions(db, did, ANCHOR_TERM)?
                    {
                        let mut anchored = Vec::with_capacity(poslists.len() + 1);
                        anchored.push(anchor_pos);
                        anchored.extend(poslists.iter().cloned());
                        if phrase_matches(&anchored, words.len() as u32 + 1) {
                            weight += phrase_weight + doc_part(aw, 1, len, len_factor);
                        }
                    }
                }
            }
        }
        matches.push(Scored { docid: did, weight });
    }

    let total = matches.len() as u32;
    // weight descending, then title ascending (bytes), then docid ascending; titles
    // are only fetched for the tie groups that reach the requested window
    matches.sort_by(|a, b| b.weight.total_cmp(&a.weight).then(a.docid.cmp(&b.docid)));
    let title_slot = db.value_slot("title")?.unwrap_or(0);
    let target_slot = db.value_slot("targetPath")?;
    let want = offset.saturating_add(limit);
    let mut hits = Vec::new();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut i = 0usize;
    while i < matches.len() && hits.len() < want {
        let mut j = i + 1;
        while j < matches.len() && matches[j].weight.to_bits() == matches[i].weight.to_bits() {
            j += 1;
        }
        let mut group: Vec<(Vec<u8>, u32, f64)> = Vec::with_capacity(j - i);
        for m in &matches[i..j] {
            group.push((
                db.value(m.docid, title_slot)?.unwrap_or_default(),
                m.docid,
                m.weight,
            ));
        }
        group.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        for (title, docid, weight) in group {
            if hits.len() >= want {
                break;
            }
            let target = match target_slot {
                Some(slot) => db.value(docid, slot)?,
                None => None,
            };
            if let Some(t) = &target
                && !t.is_empty()
                && !seen.insert(t.clone())
            {
                continue;
            }
            hits.push(Suggestion {
                docid,
                weight,
                title: String::from_utf8_lossy(&title).into_owned(),
                path: db.docdata_string(docid)?.unwrap_or_default(),
                target_path: target.map(|t| String::from_utf8_lossy(&t).into_owned()),
            });
        }
        i = j;
    }
    let hits = hits.into_iter().skip(offset).take(limit).collect();
    Ok(SuggestionResults { hits, total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_matching() {
        assert!(phrase_matches(&[vec![1], vec![2]], 2));
        assert!(phrase_matches(&[vec![5, 9], vec![10]], 2));
        assert!(!phrase_matches(&[vec![1], vec![3]], 2), "gap of one");
        assert!(
            phrase_matches(&[vec![1], vec![3]], 3),
            "allowed with a wider window"
        );
        assert!(!phrase_matches(&[vec![2], vec![1]], 2), "order matters");
        assert!(phrase_matches(&[vec![1], vec![2], vec![3]], 3));
        assert!(!phrase_matches(&[vec![1], vec![2], vec![5]], 3));
        assert!(phrase_matches(&[vec![1, 7], vec![8], vec![9]], 3));
        assert!(!phrase_matches(&[vec![], vec![1]], 2));
        assert!(phrase_matches(&[vec![4]], 1), "single term");
    }

    #[test]
    fn synonym_estimate() {
        assert_eq!(synonym_termfreq_estimate(&[], 100), 0);
        assert_eq!(synonym_termfreq_estimate(&[10], 100), 10);
        // 10 + 20 - 10*20/100 = 28
        assert_eq!(synonym_termfreq_estimate(&[10, 20], 100), 28);
        // merges the two smallest first: (5,10)->14.5->15 (rounded), then (15,20)->32
        assert_eq!(synonym_termfreq_estimate(&[20, 5, 10], 100), 32);
    }

    #[test]
    fn stemming_rules() {
        let a = Analyzer::new(Some("eng"));
        assert_eq!(stem_some(&a, "running"), "Zrun");
        assert_eq!(
            stem_some(&a, "42nd"),
            "42nd",
            "digit-initial terms are not stemmed"
        );
        assert!(!should_stem("0posanchor"));
        let none = Analyzer::new(None);
        assert_eq!(stem_some(&none, "running"), "running");
    }
}
