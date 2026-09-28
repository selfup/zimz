// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Reciprocal rank fusion. Scores from different Xapian databases are not comparable
//! (different collection statistics), so results are merged by rank: an item at
//! 0-based `rank` in a list of weight `w` scores `w / (K + rank + 1)`, plus an optional
//! boost expressed in "rank-1 units" (`boost * w / (K + 1)`).

/// The usual constant; 60 flattens the difference between ranks 1 and 2 enough that a
/// good hit from a second archive is not buried by a long run of one archive's hits.
pub const RRF_K: f64 = 60.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Fused<T> {
    pub item: T,
    pub score: f64,
    /// Which input list the item came from.
    pub list: usize,
    /// 0-based rank in that list.
    pub rank: usize,
}

/// Fuse `lists` of `(weight, ranked items)`. Items are not shared across lists (every
/// list is one archive), so no item merging is needed. Output is sorted by score
/// descending; ties keep list order then rank order, so the result is deterministic.
#[allow(clippy::cast_precision_loss)]
pub fn fuse<T>(lists: Vec<(f64, Vec<T>)>, boost: impl Fn(&T) -> f64) -> Vec<Fused<T>> {
    let mut out = Vec::new();
    for (list, (weight, items)) in lists.into_iter().enumerate() {
        let weight = if weight.is_finite() && weight > 0.0 {
            weight
        } else {
            1.0
        };
        for (rank, item) in items.into_iter().enumerate() {
            let base = weight / (RRF_K + rank as f64 + 1.0);
            let extra = boost(&item) * weight / (RRF_K + 1.0);
            out.push(Fused {
                item,
                score: base + extra,
                list,
                rank,
            });
        }
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.list.cmp(&b.list))
            .then(a.rank.cmp(&b.rank))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaves_equal_weight_lists() {
        let fused = fuse(
            vec![(1.0, vec!["a1", "a2"]), (1.0, vec!["b1", "b2"])],
            |_| 0.0,
        );
        let order: Vec<&str> = fused.iter().map(|f| f.item).collect();
        assert_eq!(order, vec!["a1", "b1", "a2", "b2"]);
    }

    #[test]
    fn priority_weight_lifts_a_list() {
        let fused = fuse(
            vec![(1.0, vec!["a1", "a2"]), (2.0, vec!["b1", "b2"])],
            |_| 0.0,
        );
        let order: Vec<&str> = fused.iter().map(|f| f.item).collect();
        assert_eq!(order, vec!["b1", "b2", "a1", "a2"]);
    }

    #[test]
    fn boost_moves_an_item_to_the_top() {
        let fused = fuse(vec![(1.0, vec!["x", "y", "z"])], |i| {
            if *i == "z" { 1.0 } else { 0.0 }
        });
        assert_eq!(fused[0].item, "z");
        assert!(fused[0].score > fused[1].score);
        assert_eq!(fused[1].item, "x");
    }

    #[test]
    fn bad_weights_fall_back_to_one() {
        let fused = fuse(vec![(f64::NAN, vec!["a"]), (0.0, vec!["b"])], |_| 0.0);
        assert!((fused[0].score - fused[1].score).abs() < 1e-12);
    }
}
