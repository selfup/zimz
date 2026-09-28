// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Text → terms exactly as libzim + Xapian produce them for the embedded indexes:
//! libzim's `removeAccents` (ICU `Lower; NFD; [:M:] remove; NFC`), Xapian's
//! `TermGenerator` tokenizer (`queryparser/termgenerator_internal.cc`) with
//! `FLAG_CJK_NGRAM`, and Snowball stemming with `STEM_ALL` (stems only, no prefix).

use std::fmt;

use rust_stemmers::{Algorithm, Stemmer};
use unicode_general_category::{GeneralCategory as Cat, get_general_category};
use unicode_normalization::UnicodeNormalization;

/// Xapian's default `max_word_length`.
pub const MAX_WORD_LENGTH: usize = 64;

pub struct Analyzer {
    stemmer: Option<Stemmer>,
    algorithm: Option<Algorithm>,
    /// Emit unigrams + bigrams for runs of unbroken scripts (CJK etc.), as
    /// `FLAG_CJK_NGRAM` does.
    pub ngrams: bool,
    pub max_word_length: usize,
}

impl fmt::Debug for Analyzer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Analyzer")
            .field("algorithm", &self.algorithm)
            .field("ngrams", &self.ngrams)
            .finish_non_exhaustive()
    }
}

impl Clone for Analyzer {
    fn clone(&self) -> Self {
        Self {
            stemmer: self.algorithm.map(Stemmer::create),
            algorithm: self.algorithm,
            ngrams: self.ngrams,
            max_word_length: self.max_word_length,
        }
    }
}

/// Map a language tag (ISO 639-3 as in ZIM metadata, ISO 639-1, or an English name) to
/// a Snowball algorithm; `None` means "no stemming", which is also what libzim ends up
/// with for languages Xapian cannot stem.
pub fn algorithm_for(language: &str) -> Option<Algorithm> {
    let lang = language
        .split([',', '-', '_'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    Some(match lang.as_str() {
        "en" | "eng" | "english" => Algorithm::English,
        "fr" | "fra" | "fre" | "french" => Algorithm::French,
        "de" | "deu" | "ger" | "german" => Algorithm::German,
        "es" | "spa" | "spanish" => Algorithm::Spanish,
        "it" | "ita" | "italian" => Algorithm::Italian,
        "pt" | "por" | "portuguese" => Algorithm::Portuguese,
        "nl" | "nld" | "dut" | "dutch" => Algorithm::Dutch,
        "sv" | "swe" | "swedish" => Algorithm::Swedish,
        "da" | "dan" | "danish" => Algorithm::Danish,
        "no" | "nor" | "nob" | "nno" | "norwegian" => Algorithm::Norwegian,
        "fi" | "fin" | "finnish" => Algorithm::Finnish,
        "hu" | "hun" | "hungarian" => Algorithm::Hungarian,
        "ro" | "ron" | "rum" | "romanian" => Algorithm::Romanian,
        "ru" | "rus" | "russian" => Algorithm::Russian,
        "tr" | "tur" | "turkish" => Algorithm::Turkish,
        "el" | "ell" | "gre" | "greek" => Algorithm::Greek,
        "ar" | "ara" | "arabic" => Algorithm::Arabic,
        "ta" | "tam" | "tamil" => Algorithm::Tamil,
        _ => return None,
    })
}

/// `Xapian::Unicode::is_wordchar`: letters, marks, numbers and connector punctuation.
pub fn is_wordchar(c: char) -> bool {
    matches!(
        get_general_category(c),
        Cat::UppercaseLetter
            | Cat::LowercaseLetter
            | Cat::TitlecaseLetter
            | Cat::ModifierLetter
            | Cat::OtherLetter
            | Cat::NonspacingMark
            | Cat::EnclosingMark
            | Cat::SpacingMark
            | Cat::DecimalNumber
            | Cat::LetterNumber
            | Cat::OtherNumber
            | Cat::ConnectorPunctuation
    )
}

fn is_digit(c: char) -> bool {
    get_general_category(c) == Cat::DecimalNumber
}

fn is_mark(c: char) -> bool {
    matches!(
        get_general_category(c),
        Cat::NonspacingMark | Cat::SpacingMark | Cat::EnclosingMark
    )
}

/// Xapian's simple lowercase mapping (one code point).
fn to_lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Scripts Xapian treats as "unbroken" (no word separators): CJK, Hangul, fullwidth…
pub fn is_unbroken_script(c: char) -> bool {
    let p = c as u32;
    if p < 0x2E80 {
        return false;
    }
    (0x2E80..=0x2EFF).contains(&p)
        || (0x3000..=0x9FFF).contains(&p)
        || (0xA700..=0xA71F).contains(&p)
        || (0xAC00..=0xD7AF).contains(&p)
        || (0xF900..=0xFAFF).contains(&p)
        || (0xFE30..=0xFE4F).contains(&p)
        || (0xFF00..=0xFFEF).contains(&p)
        || (0x20000..=0x2A6DF).contains(&p)
        || (0x2F800..=0x2FA1F).contains(&p)
}

/// What to do with a character found between two word characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Infix {
    /// Append this character and keep going (`'`, `&`, `.` between digits, …).
    Keep(char),
    /// Zero-width characters: skip and keep going.
    Ignore,
    /// Anything else ends the term.
    End,
}

fn check_infix(c: char) -> Infix {
    match c {
        '\'' | '&' | '\u{b7}' | '\u{5f4}' | '\u{2027}' => Infix::Keep(c),
        '\u{2019}' | '\u{201b}' => Infix::Keep('\''),
        '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' => Infix::Ignore,
        _ => Infix::End,
    }
}

fn check_infix_digit(c: char) -> Infix {
    match c {
        ',' | '.' | ';' | '\u{037e}' | '\u{0589}' | '\u{060d}' | '\u{07f8}' | '\u{2044}'
        | '\u{fe10}' | '\u{fe13}' | '\u{fe14}' => Infix::Keep(c),
        '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' => Infix::Ignore,
        _ => Infix::End,
    }
}

fn check_suffix(c: char) -> Option<char> {
    matches!(c, '+' | '#').then_some(c)
}

impl Analyzer {
    /// An analyzer for a ZIM/Xapian language tag (`None` or unknown → no stemming).
    pub fn new(language: Option<&str>) -> Self {
        let algorithm = language.and_then(algorithm_for);
        Self {
            stemmer: algorithm.map(Stemmer::create),
            algorithm,
            ngrams: true,
            max_word_length: MAX_WORD_LENGTH,
        }
    }

    pub fn algorithm(&self) -> Option<Algorithm> {
        self.algorithm
    }

    /// libzim's `removeAccents`: lowercase, decompose, drop all marks, recompose.
    pub fn remove_accents(text: &str) -> String {
        text.to_lowercase()
            .nfd()
            .filter(|c| !is_mark(*c))
            .nfc()
            .collect()
    }

    pub fn stem(&self, term: &str) -> String {
        match &self.stemmer {
            Some(s) => s.stem(term).into_owned(),
            None => term.to_string(),
        }
    }

    /// Xapian's `parse_terms`: lowercased tokens, no stemming.
    #[allow(clippy::too_many_lines)]
    pub fn tokenize(&self, text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        let mut out = Vec::new();
        let mut i = 0usize;
        'terms: loop {
            while i < n && !is_wordchar(chars[i]) {
                i += 1;
            }
            if i >= n {
                return out;
            }
            let mut term = String::new();
            // initials separated by '.' (P.T.O., U.N.C.L.E.)
            if chars[i].is_ascii_uppercase() {
                let mut p = i;
                let mut count = 0;
                loop {
                    term.push(to_lower(chars[p]));
                    count += 1;
                    p += 1;
                    if p < n && chars[p] == '.' {
                        p += 1;
                        if p < n && chars[p].is_ascii_uppercase() {
                            continue;
                        }
                    }
                    break;
                }
                if count > 1 && (p >= n || !is_wordchar(chars[p])) {
                    i = p;
                    out.push(term);
                    continue 'terms;
                }
                term.clear();
            }
            let mut ch = to_lower(chars[i]);
            loop {
                if self.ngrams && is_unbroken_script(chars[i]) && is_wordchar(chars[i]) {
                    // unigrams and bigrams over the whole unbroken run
                    let mut run = Vec::new();
                    while i < n && is_unbroken_script(chars[i]) && is_wordchar(chars[i]) {
                        run.push(chars[i]);
                        i += 1;
                    }
                    for (k, &c) in run.iter().enumerate() {
                        out.push(c.to_string());
                        if let Some(&d) = run.get(k + 1) {
                            out.push(format!("{c}{d}"));
                        }
                    }
                    while i < n && !is_wordchar(chars[i]) {
                        i += 1;
                    }
                    if i >= n {
                        return out;
                    }
                    ch = to_lower(chars[i]);
                    term.clear();
                    continue;
                }
                let mut prevch;
                let mut ended = false;
                loop {
                    term.push(ch);
                    prevch = ch;
                    i += 1;
                    if i >= n || (self.ngrams && is_unbroken_script(chars[i])) {
                        ended = true;
                        break;
                    }
                    if !is_wordchar(chars[i]) {
                        break;
                    }
                    ch = to_lower(chars[i]);
                }
                if ended {
                    break;
                }
                let next = i + 1;
                if next >= n || !is_wordchar(chars[next]) {
                    break;
                }
                let infix = if is_digit(prevch) && is_digit(chars[next]) {
                    check_infix_digit(chars[i])
                } else {
                    check_infix(chars[i])
                };
                match infix {
                    Infix::End => break,
                    Infix::Ignore => {}
                    Infix::Keep(c) => term.push(c),
                }
                ch = to_lower(chars[next]);
                i = next;
            }
            if i < n {
                // up to three trailing '+' / '#' (C++, C#), unless a word char follows
                let len = term.len();
                let mut count = 0;
                let mut cut = false;
                while i < n {
                    let Some(s) = check_suffix(chars[i]) else {
                        break;
                    };
                    count += 1;
                    if count > 3 {
                        term.truncate(len);
                        cut = true;
                        break;
                    }
                    term.push(s);
                    i += 1;
                }
                if !cut && i < n && is_wordchar(chars[i]) {
                    term.truncate(len);
                }
            }
            out.push(term);
        }
    }

    /// Normalise, tokenize and stem: the exact terms libzim indexed (`STEM_ALL`).
    pub fn terms(&self, text: &str) -> Vec<String> {
        let normalized = Self::remove_accents(text);
        let mut out = Vec::new();
        for term in self.tokenize(&normalized) {
            if term.len() > self.max_word_length {
                continue;
            }
            let stem = self.stem(&term);
            if !stem.is_empty() {
                out.push(stem);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_and_case() {
        assert_eq!(
            Analyzer::remove_accents("Ça Résumé Ångström"),
            "ca resume angstrom"
        );
        assert_eq!(Analyzer::remove_accents("ÉCOLE"), "ecole");
        assert_eq!(Analyzer::remove_accents("naïve 東京"), "naive 東京");
    }

    #[test]
    fn xapian_tokenizer_rules() {
        let a = Analyzer::new(None);
        assert_eq!(a.tokenize("Hello, World!"), ["hello", "world"]);
        assert_eq!(a.tokenize("AT&T isn't don’t"), ["at&t", "isn't", "don't"]);
        assert_eq!(a.tokenize("3.14 1,000 v1.2.3"), ["3.14", "1,000", "v1.2.3"]);
        assert_eq!(
            a.tokenize("C++ C# fish+chips A+++++"),
            ["c++", "c#", "fish", "chips", "a"]
        );
        assert_eq!(
            a.tokenize("snake_case api-trace2"),
            ["snake_case", "api", "trace2"]
        );
        assert_eq!(a.tokenize("U.N.C.L.E. agent"), ["uncle", "agent"]);
        assert_eq!(a.tokenize("x."), ["x"]);
        assert_eq!(
            a.tokenize("word\u{200b}break"),
            ["wordbreak"],
            "zero-width space is ignored inside a word"
        );
        assert!(a.tokenize("   ...  ").is_empty());
    }

    #[test]
    fn cjk_ngrams() {
        let a = Analyzer::new(None);
        assert_eq!(a.tokenize("東京都"), ["東", "東京", "京", "京都", "都"]);
        assert_eq!(a.tokenize("go東京now"), ["go", "東", "東京", "京", "now"]);
        let mut b = Analyzer::new(None);
        b.ngrams = false;
        assert_eq!(b.tokenize("東京都"), ["東京都"]);
    }

    #[test]
    fn stemming() {
        let a = Analyzer::new(Some("eng"));
        assert_eq!(a.terms("Running runners ran"), ["run", "runner", "ran"]);
        assert_eq!(
            a.terms("The Documentation of Git's repositories"),
            ["the", "document", "of", "git", "repositori"]
        );
        assert_eq!(a.terms("Ångström"), ["angstrom"]);
        let long = "x".repeat(65);
        assert!(a.terms(&long).is_empty(), "over max word length");
        assert_eq!(
            Analyzer::new(Some("fra")).terms("Les maisons"),
            ["le", "maison"]
        );
        assert!(Analyzer::new(Some("xx")).algorithm().is_none());
        assert_eq!(Analyzer::new(None).terms("Running"), ["running"]);
        assert_eq!(algorithm_for("eng,fra"), Some(Algorithm::English));
    }
}
