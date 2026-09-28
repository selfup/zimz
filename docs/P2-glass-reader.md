# P2 — `zimz-glass`: native Xapian glass reader and search (2026-09-24)

Scope: phase P2. Reads the Xapian "glass" single-file databases that
libzim embeds in every ZIM (`X/fulltext/xapian`, `X/title/xapian`) without linking
Xapian, and reproduces libzim's full-text search and title suggestions.

## What exists (crate `zimz-glass`, ~2 900 lines incl. tests)

| Module | Ported from (xapian-core 1.4.31, GPL-2.0-or-later) |
|---|---|
| `pack` | `common/pack.h`: LEB128, sort-preserving integers, escaped strings |
| `version` | `glass_version.cc`: magic, format 2016-03-14, six `RootInfo`s, stats |
| `table` | `glass_table.{h,cc}`, `glass_cursor.cc`: block/item layout, key comparison, branch/leaf search, split tags, raw-deflate tags |
| `postlist` | `glass_postlist.cc`: chunked postings, `skip_to`, the document-length list |
| `position` | `glass_positionlist.cc`, `bitstream.cc`: interpolative-coded positions |
| `db` | metadata (`\0\xc0`), docdata, value chunks (`\0\xd8`) and stats (`\0\xd0`), term iteration by prefix |
| `analyzer` | libzim `removeAccents` + Xapian `TermGenerator` tokenizer (infix rules, acronyms, `+`/`#` suffixes, CJK uni/bigrams) + Snowball via `rust-stemmers` |
| `search` | libzim `search.cpp` semantics: AND of `STEM_ALL` stems, BM25(k1=1, b=0.5), Xapian ordering and percent; OR mode as an extension |
| `suggest` | libzim `suggestion.cpp`: AND of `Z`-stems with the last word as a 100-most-frequent wildcard synonym, OR exact phrase, OR title-anchored phrase; BM25(k1=0.001, b=1); sort by weight then title; collapse on `targetPath` |

Zero-copy: the database is a byte slice of the memory-mapped ZIM; B-tree blocks are
`&data[n * 8192..]`. No allocation beyond tags and result sets.

## Verification

| Check | Result |
|---|---|
| Term-level vs `xapian-delve` (Xapian 2.1 CLI on the extracted blob) | postings `(docid, wdf, doclen)`, term stats, prefix listings and docdata identical on the fixture index |
| Full-text ranking vs python-libzim `Searcher` (`scripts/search_parity.py`) | **11 archives × 15–20 queries: identical top-10 order for every query** (ZIM 5.0 wikem/AoPS, 6.2 ifixit/libretexts, 6.3 devdocs/mankier/bulbagarden/proofwiki/wikispecies/Wikipedia + fixture). Totals equal libzim's estimate except where Xapian's estimate stops early (e.g. 118 vs "200") |
| Suggestions vs python-libzim `SuggestionSearcher` (`scripts/suggest_parity.py`) | **6 archives × 10–16 prefixes: identical order for 79/80**; the one difference (wikispecies "war") sits at the boundary of Xapian's 100-expansion cap, whose tie-break (`nth_element`) is unspecified |
| Structure | 18 unit tests (encodings round-trip against a ported encoder, key comparison, phrase matching, BM25 arithmetic), 8 fixture tests over all three archive flavours (doc lengths sum to the stats, every docdata/value readable, term iteration sorted, `skip_to` semantics, AND/OR set algebra, paging) |

## Timings (release, warm page cache)

| Query | Archive | Latency |
|---|---|---|
| `rebase branch` (AND, 41 hits) | devdocs git (0.3 MB index) | 30 µs |
| `chest pain` (290 hits) | wikem, ZIM 5.0 | 75 µs |
| `felis` (296 hits) | wikispecies (461 MB index) | 1.0 ms |
| `quantum mechanics` (13 105 hits) | Wikipedia (8.2 GB index) | 12–35 ms |
| `leonardo da vinci painting` (3 101 hits) | Wikipedia | 12 ms |
| suggest `git re` | devdocs | 85 µs |
| suggest `leonardo da` (277 titles) | Wikipedia (3.5 GB title index) | 75 ms |
| suggest `quantum` (2 179 titles) | Wikipedia | 256 ms |

The full-text target (< 50 ms warm) is met. Single-word suggestions on
Wikipedia are above the 20 ms target: every candidate costs two position-list lookups
and the tie groups need title values. Open item in PLAN.md §2; candidates: skip the
phrase checks when the query is one word that cannot form a phrase, batch title reads,
cap prefixes at two characters in the MCP layer.

## Semantics worth knowing

- Queries are plain words, like libzim's: no boolean operators, quotes or wildcards in
  full-text search (libzim parses with only `FLAG_CJK_NGRAM`). `Op::Or` is our addition.
- `SearchResults::total` is exact; libzim reports Xapian's estimate.
- Suggestions: `total` counts matches before redirect collapsing. A prefix's expansions
  are the 100 most frequent terms, as in Xapian; results for very short prefixes on huge
  archives are therefore "most frequent 100 terms starting with x", not all of them.
- Stemmers come from `rust-stemmers` (Snowball); Xapian ships its own Snowball snapshot.
  No difference showed up in 80 English/Latin queries, but other languages are untested.
- Old (2021) title indexes have no `targetPath` value, so nothing is collapsed there,
  exactly as libzim behaves.

## CLI

`zimz search <zim> "<query>" [-n N] [--offset K] [--any] [--time]` and
`zimz suggest <zim> "<prefix>" [-n N] [--time]`.
