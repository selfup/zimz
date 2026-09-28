// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Query evaluation with Xapian's BM25 (`weight/bm25weight.cc`, default parameters)
//! and result ordering (weight descending, then docid ascending).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::analyzer::Analyzer;
use crate::db::GlassDb;
use crate::postlist::PostList;
use crate::{Error, Result};

const K1: f64 = 1.0;
const K3: f64 = 1.0;
const B: f64 = 0.5;
const MIN_NORMLEN: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Every term must match (libzim's default).
    And,
    /// Any term may match.
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryTerm {
    pub term: String,
    /// Within-query frequency.
    pub wqf: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub terms: Vec<QueryTerm>,
    pub op: Op,
}

impl Query {
    /// Analyse free text into stemmed terms (duplicates fold into `wqf`).
    pub fn parse(analyzer: &Analyzer, text: &str, op: Op) -> Self {
        let mut terms: Vec<QueryTerm> = Vec::new();
        for t in analyzer.terms(text) {
            match terms.iter_mut().find(|q| q.term == t) {
                Some(q) => q.wqf += 1,
                None => terms.push(QueryTerm { term: t, wqf: 1 }),
            }
        }
        Self { terms, op }
    }

    pub fn from_terms(terms: impl IntoIterator<Item = String>, op: Op) -> Self {
        Self {
            terms: terms
                .into_iter()
                .map(|term| QueryTerm { term, wqf: 1 })
                .collect(),
            op,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub docid: u32,
    pub weight: f64,
    /// Xapian-style percentage relative to the best hit (1..=100).
    pub percent: u8,
    /// Query terms matching this document.
    pub matched_terms: u32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SearchResults {
    /// Hits `offset..offset+limit`, best first.
    pub hits: Vec<Hit>,
    /// Exact number of matching documents.
    pub total: u32,
    pub max_weight: f64,
}

/// Xapian's BM25 term weight, including the `(k1 + 1)` factor and `wqf` scaling.
#[allow(clippy::cast_precision_loss)]
pub fn term_weight(collection_size: u32, term_freq: u32, wqf: u32) -> f64 {
    let mut tw =
        (f64::from(collection_size) - f64::from(term_freq) + 0.5) / (f64::from(term_freq) + 0.5);
    if tw < 2.0 {
        tw = tw * 0.5 + 1.0;
    }
    let mut w = tw.ln();
    if K3 != 0.0 {
        let wqf = f64::from(wqf);
        w *= (K3 + 1.0) * wqf / (K3 + wqf);
    }
    w * (K1 + 1.0)
}

/// One document's contribution for one term.
pub fn doc_part(term_weight: f64, wdf: u32, doc_len: u32, len_factor: f64) -> f64 {
    let normlen = (f64::from(doc_len) * len_factor).max(MIN_NORMLEN);
    let wdf = f64::from(wdf);
    let denom = K1 * (normlen * B + (1.0 - B)) + wdf;
    term_weight * (wdf / denom)
}

#[derive(Debug, Clone, Copy)]
struct Scored {
    docid: u32,
    weight: f64,
    matched: u32,
}

impl Scored {
    /// Xapian order: higher weight first, then lower docid first.
    fn better_than(&self, other: &Self) -> bool {
        self.weight
            .total_cmp(&other.weight)
            .then(other.docid.cmp(&self.docid))
            == Ordering::Greater
    }
}

/// Min-heap of the best `k` by `better_than` (the worst kept entry is at the top).
struct TopK {
    k: usize,
    heap: BinaryHeap<HeapEntry>,
}

struct HeapEntry(Scored);

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.0.docid == other.0.docid && self.0.weight.total_cmp(&other.0.weight) == Ordering::Equal
    }
}
impl Eq for HeapEntry {}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapEntry {
    // reversed so that the *worst* hit is the heap maximum
    fn cmp(&self, other: &Self) -> Ordering {
        if self.0.better_than(&other.0) {
            Ordering::Less
        } else if other.0.better_than(&self.0) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }
}

impl TopK {
    fn new(k: usize) -> Self {
        Self {
            k,
            heap: BinaryHeap::with_capacity(k + 1),
        }
    }

    fn push(&mut self, s: Scored) {
        if self.k == 0 {
            return;
        }
        if self.heap.len() < self.k {
            self.heap.push(HeapEntry(s));
        } else if let Some(worst) = self.heap.peek()
            && s.better_than(&worst.0)
        {
            self.heap.pop();
            self.heap.push(HeapEntry(s));
        }
    }

    fn into_sorted(self) -> Vec<Scored> {
        let mut v: Vec<Scored> = self.heap.into_iter().map(|e| e.0).collect();
        v.sort_by(|a, b| {
            if a.better_than(b) {
                Ordering::Less
            } else if b.better_than(a) {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        });
        v
    }
}

struct TermList<'a> {
    pl: PostList<'a>,
    weight: f64,
}

/// Accumulates matches: exact count, best hit, and the top `offset + limit`.
struct Collector {
    top: TopK,
    total: u32,
    best: Option<Scored>,
}

impl Collector {
    fn record(&mut self, docid: u32, weight: f64, matched: u32) {
        self.total += 1;
        let s = Scored {
            docid,
            weight,
            matched,
        };
        if self.best.is_none_or(|b| s.better_than(&b)) {
            self.best = Some(s);
        }
        self.top.push(s);
    }
}

/// Leapfrog intersection over the lists (sorted by term frequency, rarest first).
fn eval_and(
    lists: &mut [TermList<'_>],
    doclens: &mut PostList<'_>,
    len_factor: f64,
    out: &mut Collector,
) -> Result<()> {
    lists.sort_by_key(|l| l.pl.term_freq());
    'outer: loop {
        if lists[0].pl.at_end() {
            break;
        }
        let mut cand = lists[0].pl.docid();
        let mut i = 1;
        while i < lists.len() {
            if !lists[i].pl.skip_to(cand)? {
                break 'outer;
            }
            let d = lists[i].pl.docid();
            if d != cand {
                cand = d;
                if !lists[0].pl.skip_to(cand)? {
                    break 'outer;
                }
                if lists[0].pl.docid() != cand {
                    cand = lists[0].pl.docid();
                }
                i = 1;
                continue;
            }
            i += 1;
        }
        let len = if doclens.jump_to(cand)? {
            doclens.wdf()
        } else {
            0
        };
        let weight: f64 = lists
            .iter()
            .map(|l| doc_part(l.weight, l.pl.wdf(), len, len_factor))
            .sum();
        out.record(cand, weight, lists.len() as u32);
        if !lists[0].pl.advance()? {
            break;
        }
    }
    Ok(())
}

/// Document-at-a-time union driven by a heap of the lists' current docids.
fn eval_or(
    lists: &mut [TermList<'_>],
    doclens: &mut PostList<'_>,
    len_factor: f64,
    out: &mut Collector,
) -> Result<()> {
    let mut heap: BinaryHeap<std::cmp::Reverse<(u32, usize)>> = BinaryHeap::new();
    for (i, l) in lists.iter().enumerate() {
        if !l.pl.at_end() {
            heap.push(std::cmp::Reverse((l.pl.docid(), i)));
        }
    }
    while let Some(std::cmp::Reverse((did, first))) = heap.pop() {
        let mut members = vec![first];
        while let Some(std::cmp::Reverse((d, i))) = heap.peek().copied() {
            if d != did {
                break;
            }
            heap.pop();
            members.push(i);
        }
        let len = if doclens.jump_to(did)? {
            doclens.wdf()
        } else {
            0
        };
        let mut weight = 0.0;
        for &i in &members {
            let l = &mut lists[i];
            weight += doc_part(l.weight, l.pl.wdf(), len, len_factor);
            if l.pl.advance()? {
                heap.push(std::cmp::Reverse((l.pl.docid(), i)));
            }
        }
        out.record(did, weight, members.len() as u32);
    }
    Ok(())
}

/// Xapian's `convert_to_percent`: relative to the best hit, scaled by the fraction of
/// query terms the best hit matched.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]
fn percent_of(weight: f64, best: Option<Scored>, query_terms: usize) -> u8 {
    let factor = match best {
        Some(b) if b.weight > 0.0 => (f64::from(b.matched) / query_terms as f64) * 100.0 / b.weight,
        _ => return 100,
    };
    let v = weight * factor + 100.0 * f64::EPSILON;
    let mut p = v.clamp(0.0, 100.0) as u8;
    if p == 0 && weight > 0.0 {
        p = 1;
    }
    p
}

/// Evaluate `query` and return hits `offset..offset+limit` in Xapian order.
pub fn search(
    db: &GlassDb<'_>,
    query: &Query,
    offset: usize,
    limit: usize,
) -> Result<SearchResults> {
    if query.terms.is_empty() {
        return Ok(SearchResults::default());
    }
    let n = db.doc_count();
    let avg = db.avg_doclen();
    let len_factor = if avg > 0.0 { 1.0 / avg } else { 0.0 };
    let mut lists = Vec::with_capacity(query.terms.len());
    for qt in &query.terms {
        match db.postlist(&qt.term)? {
            Some(pl) => {
                let weight = term_weight(n, pl.term_freq(), qt.wqf);
                lists.push(TermList { pl, weight });
            }
            None if query.op == Op::And => return Ok(SearchResults::default()),
            None => {}
        }
    }
    if lists.is_empty() {
        return Ok(SearchResults::default());
    }
    let mut doclens = db
        .doc_lengths()?
        .ok_or_else(|| Error::corrupt("database has no document lengths"))?;
    let mut out = Collector {
        top: TopK::new(offset.saturating_add(limit)),
        total: 0,
        best: None,
    };
    match query.op {
        Op::And => eval_and(&mut lists, &mut doclens, len_factor, &mut out)?,
        Op::Or => eval_or(&mut lists, &mut doclens, len_factor, &mut out)?,
    }
    let best = out.best;
    let max_weight = best.map_or(0.0, |b| b.weight);
    let hits = out
        .top
        .into_sorted()
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|s| Hit {
            docid: s.docid,
            weight: s.weight,
            percent: percent_of(s.weight, best, query.terms.len()),
            matched_terms: s.matched,
        })
        .collect();
    Ok(SearchResults {
        hits,
        total: out.total,
        max_weight,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm25_matches_xapian_formula() {
        // N = 205, tf = 10, wqf = 1: tw = (205-10+0.5)/(10.5) = 18.619; ln = 2.924; ×(k3+1)wqf/(k3+wqf) = ×1; ×(k1+1) = 5.848
        let w = term_weight(205, 10, 1);
        assert!((w - 2.0 * ((205.0 - 10.0 + 0.5f64) / 10.5).ln()).abs() < 1e-12);
        // very common term: tw < 2 → tw*0.5 + 1 keeps the weight positive
        let common = term_weight(100, 90, 1);
        assert!(common > 0.0 && common < w);
        // repeated query term is boosted: (k3+1)*2/(k3+2) = 4/3
        assert!((term_weight(205, 10, 2) / w - 4.0 / 3.0).abs() < 1e-12);
        // doc part: avg len 100, doc len 100 → normlen 1 → denom = k1*(0.5+0.5) + wdf
        let p = doc_part(w, 3, 100, 0.01);
        assert!((p - w * 3.0 / 4.0).abs() < 1e-12);
        // short docs are clamped at min_normlen
        assert!(
            (doc_part(w, 1, 1, 0.01) - w * 1.0 / (1.0 * (0.5 * 0.5 + 0.5) + 1.0)).abs() < 1e-12
        );
    }

    #[test]
    fn top_k_keeps_best_with_xapian_tiebreak() {
        let mut t = TopK::new(2);
        t.push(Scored {
            docid: 5,
            weight: 1.0,
            matched: 1,
        });
        t.push(Scored {
            docid: 3,
            weight: 1.0,
            matched: 1,
        });
        t.push(Scored {
            docid: 9,
            weight: 2.0,
            matched: 1,
        });
        t.push(Scored {
            docid: 1,
            weight: 0.5,
            matched: 1,
        });
        let v: Vec<(u32, f64)> = t
            .into_sorted()
            .iter()
            .map(|s| (s.docid, s.weight))
            .collect();
        assert_eq!(
            v,
            vec![(9, 2.0), (3, 1.0)],
            "equal weights: lower docid first"
        );
        let mut z = TopK::new(0);
        z.push(Scored {
            docid: 1,
            weight: 1.0,
            matched: 1,
        });
        assert!(z.into_sorted().is_empty());
    }
}
