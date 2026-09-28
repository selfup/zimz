// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Query-focused snippets: the window of at most `max_chars` that covers the most
//! distinct query terms, cut on word boundaries, with optional highlighting.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    /// Byte range of the snippet in the source text.
    pub start: usize,
    pub end: usize,
    /// Distinct query terms inside the window.
    pub matched_terms: usize,
}

struct Word {
    start: usize,
    end: usize,
    term: Option<usize>,
}

fn words(text: &str, term_index: &impl Fn(&str) -> Option<usize>) -> Vec<Word> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() || c == '_' || c == '\'' {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            out.push(Word {
                start: s,
                end: i,
                term: term_index(&text[s..i]),
            });
        }
    }
    if let Some(s) = start {
        out.push(Word {
            start: s,
            end: text.len(),
            term: term_index(&text[s..]),
        });
    }
    out
}

/// `(first word, last word, (distinct terms, total matches))` of the best window.
fn best_window(
    text: &str,
    ws: &[Word],
    max_chars: usize,
    mark_cost: usize,
) -> (usize, usize, (usize, usize)) {
    // prefix[i] = matched words among ws[..i]; highlight markers cost `mark_cost` each
    let mut prefix = Vec::with_capacity(ws.len() + 1);
    prefix.push(0usize);
    for w in ws {
        prefix.push(prefix.last().copied().unwrap_or(0) + usize::from(w.term.is_some()));
    }
    let fits = |lo: usize, hi: usize| {
        text[ws[lo].start..ws[hi].end].chars().count() + (prefix[hi + 1] - prefix[lo]) * mark_cost
            <= max_chars
    };
    // sliding window over words: [lo, hi] fits in max_chars
    let (mut best_lo, mut best_hi, mut best_score) = (0usize, 0usize, (0usize, 0usize));
    let mut lo = 0usize;
    let mut counts: Vec<usize> = Vec::new();
    let mut distinct = 0usize;
    let mut total = 0usize;
    for hi in 0..ws.len() {
        if let Some(t) = ws[hi].term {
            if counts.len() <= t {
                counts.resize(t + 1, 0);
            }
            if counts[t] == 0 {
                distinct += 1;
            }
            counts[t] += 1;
            total += 1;
        }
        while lo < hi && !fits(lo, hi) {
            if let Some(t) = ws[lo].term {
                counts[t] -= 1;
                if counts[t] == 0 {
                    distinct -= 1;
                }
                total -= 1;
            }
            lo += 1;
        }
        let score = (distinct, total);
        if score > best_score {
            best_score = score;
            best_lo = lo;
            best_hi = hi;
        }
    }
    if best_score.0 == 0 {
        // no match: the beginning of the text
        best_lo = 0;
        best_hi = 0;
        while best_hi + 1 < ws.len() && fits(0, best_hi + 1) {
            best_hi += 1;
        }
    } else {
        // grow the window with context on both sides while it fits
        loop {
            let mut grew = false;
            if best_hi + 1 < ws.len() && fits(best_lo, best_hi + 1) {
                best_hi += 1;
                grew = true;
            }
            if best_lo > 0 && fits(best_lo - 1, best_hi) {
                best_lo -= 1;
                grew = true;
            }
            if !grew {
                break;
            }
        }
    }
    (best_lo, best_hi, best_score)
}

/// Pick the best window. `term_index(word)` returns the index of the query term the
/// word matches (after the caller's own normalisation and stemming), or `None`.
/// `highlight` wraps matched words, e.g. `("**", "**")`.
pub fn best_snippet(
    text: &str,
    term_index: impl Fn(&str) -> Option<usize>,
    max_chars: usize,
    highlight: Option<(&str, &str)>,
) -> Snippet {
    best_snippet_with(text, term_index, max_chars, highlight, true)
}

/// Runs of three or more newlines become one blank line.
fn squeeze_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newlines = 0;
    for c in text.chars() {
        if c == '\n' {
            newlines += 1;
            if newlines <= 2 {
                out.push(c);
            }
        } else {
            newlines = 0;
            out.push(c);
        }
    }
    out
}

/// [`best_snippet`] with control over whitespace: `collapse_whitespace = false` keeps
/// line structure (for Markdown excerpts) instead of joining everything on one line.
pub fn best_snippet_with(
    text: &str,
    term_index: impl Fn(&str) -> Option<usize>,
    max_chars: usize,
    highlight: Option<(&str, &str)>,
    collapse_whitespace: bool,
) -> Snippet {
    let ws = words(text, &term_index);
    if ws.is_empty() {
        let end = text
            .char_indices()
            .nth(max_chars)
            .map_or(text.len(), |(i, _)| i);
        return Snippet {
            text: text[..end].trim().to_string(),
            start: 0,
            end,
            matched_terms: 0,
        };
    }
    let mark_cost = highlight.map_or(0, |(o, c)| o.chars().count() + c.chars().count());
    // reserve room for the ellipses so the result never exceeds `max_chars`
    let budget = max_chars.saturating_sub(2).max(1);
    let (best_lo, best_hi, best_score) = best_window(text, &ws, budget, mark_cost);
    // Keep the boundary tokens whole: "(word" and "word)." stay attached.
    let start = text[..ws[best_lo].start]
        .rfind(char::is_whitespace)
        .map_or(0, |i| i + 1);
    let end = text[ws[best_hi].end..]
        .find(char::is_whitespace)
        .map_or(text.len(), |i| ws[best_hi].end + i);
    let mut out = String::with_capacity(end - start + 16);
    if !text[..start].trim().is_empty() {
        out.push('…');
    }
    match highlight {
        None => out.push_str(&text[start..end]),
        Some((open, close)) => {
            let mut pos = start;
            for w in &ws[best_lo..=best_hi] {
                out.push_str(&text[pos..w.start]);
                if w.term.is_some() {
                    out.push_str(open);
                    out.push_str(&text[w.start..w.end]);
                    out.push_str(close);
                } else {
                    out.push_str(&text[w.start..w.end]);
                }
                pos = w.end;
            }
            out.push_str(&text[pos..end]);
        }
    }
    if !text[end..].trim().is_empty() {
        out.push('…');
    }
    let mut text_out = if collapse_whitespace {
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        squeeze_blank_lines(out.trim())
    };
    // boundary-token extension can add a few characters; enforce the contract
    if text_out.chars().count() > max_chars {
        let cut = text_out
            .char_indices()
            .nth(max_chars.saturating_sub(1))
            .map_or(text_out.len(), |(i, _)| i);
        text_out.truncate(cut);
        text_out.push('…');
    }
    Snippet {
        text: text_out,
        start,
        end,
        matched_terms: best_score.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx<'a>(terms: &'a [&str]) -> impl Fn(&str) -> Option<usize> + 'a {
        move |w| terms.iter().position(|t| t.eq_ignore_ascii_case(w))
    }

    #[test]
    fn picks_the_densest_window() {
        let text = "Intro sentence without anything. Later the quick brown fox jumps over the lazy dog, and the fox again. Trailing words here.";
        let s = best_snippet(text, idx(&["fox", "dog"]), 40, Some(("**", "**")));
        assert_eq!(s.matched_terms, 2);
        assert!(
            s.text.contains("**fox**") && s.text.contains("**dog**"),
            "{}",
            s.text
        );
        assert!(s.text.starts_with('…') && s.text.ends_with('…'));
        assert!(
            text[s.start..s.end].chars().count() <= 40,
            "window {:?}",
            &text[s.start..s.end]
        );
    }

    #[test]
    fn no_match_takes_the_start() {
        let s = best_snippet("one two three four five six", idx(&["zzz"]), 12, None);
        assert_eq!(s.text, "one two…");
        assert_eq!(s.matched_terms, 0);
        assert_eq!(best_snippet("", idx(&["a"]), 10, None).text, "");
        assert_eq!(
            best_snippet("!!! ...", idx(&["a"]), 10, None).text,
            "!!! ..."
        );
    }

    #[test]
    fn whole_text_when_it_fits() {
        let s = best_snippet("a fox here", idx(&["fox"]), 100, Some(("<b>", "</b>")));
        assert_eq!(s.text, "a <b>fox</b> here");
        assert_eq!((s.start, s.end), (0, 10));
    }
}
