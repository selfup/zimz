# P4 — `zimz-search` + `zimz-mcp`: a library of archives, served to agents (2026-09-24)

Scope: PLAN.md §6.6, §6.7, §7 P4. One directory of ZIM files becomes one searchable
library; an MCP server on stdio exposes it to AI agents with eight read-only tools and
`zim://` resources.

## `zimz-search` (~2 300 lines incl. tests)

| Module | Role |
|---|---|
| `library` | `Library::scan(LibraryConfig)`: finds `*.zim` and the first part of split archives (recursive by default, symlinks followed), opens all of them in parallel (rayon), records failures instead of aborting. Each `Slot` keeps the `Archive`, its `Analyzer` (language from the index metadata, else `Language`), the extraction adapter and the location/value-slot layout of both embedded Xapian databases (re-opened per query: `GlassDb::open` is ~2 µs). Unique names: `Name` metadata, else the file stem, else `name~2`. `select(patterns)` accepts names, uuids, file stems, case-insensitive globs and comma lists. `resolve_entry` takes anything an agent may hand back: `zim://` URIs, `C/…`/`A/…`, bare paths, percent-encoded or with spaces, titles, fragments; redirects are followed and reported. Extract cache: LRU bounded in bytes, keyed by entry, separate entries for capped extractions. |
| `catalog` | `ArchiveInfo`: name, file, uuid, title, description, language, creator, publisher, date, flavour, scraper, tags, `_category`, size, entry/article/media counts, index presence and document counts, the search mode that will be used, priority, main page, ZIM version, adapter. |
| `search` | Per archive: full-text AND via `zimz-glass` (top `depth` = page end, 10..500), or the title index (`suggest`), or a prefix scan of the title listing (as typed, then capitalised) when the archive has no Xapian index at all. Fusion in `fusion`: reciprocal rank fusion (k = 60) weighted by the archive priority, plus boosts in rank-1 units: +1.0 exact title match, +0.5 all query stems in the title, +0.75 × match strength (raw BM25 relative to the strongest hit of the whole federation; half-strength scaled by in-archive percent for title-index hits, 0.25 for listing hits). The strength term is what keeps a weak rank-1 hit from a 1-hit archive from tying with Wikipedia's rank-1. OR fallback when AND fills less than a page (multi-word queries, full-text archives only): OR-only hits are appended after all AND hits, scored below them, flagged `partial`. Scores are normalised to the best hit = 1.0. Cursor = hash(query, archives, mode, flags) + offset, validated. Snippets only for the page: best window of the extracted text with `**term**` marks, never longer than `snippet_chars`. Titles on the page come from the directory (the index stores lowercased copies in newer mwoffliner archives). |
| `suggest` | Same federation over the title indexes (listing prefix scan as fallback), exact-title boost, `limit` ≤ 50. |
| `article` | `read_article`: Markdown, text or raw HTML window of `max_chars` characters from `offset`, with the outline whenever the article was cut, or one `section` by index/title prefix; `outline`; `links` (anchor text, canonical target path, title, existence). |
| `context` | Search (2 × `max_hits`, no snippets), extract every hit in parallel from at most `max_snippet_source_bytes` (4 MiB; Gutenberg books are 20 MB+), choose the best excerpt among the lead and each section (most distinct terms, then most specific section, then earliest), pack in rank order under `budget_chars` (an excerpt is trimmed at a word boundary if ≥ 200 chars remain, else packing stops). Each excerpt cites `zim://archive/path#Section`. `markdown` holds the packed rendering. |
| `health` | Index presence and coverage (`fulltext_docs / article_count`), open time, cluster and extract cache statistics, scan failures; `verify: quick` runs the structural checks, `full` adds the MD5 and every cluster (opt-in: minutes on Wikipedia). |

All request/response types are `serde` data with doc comments; the `schema` feature adds
`schemars::JsonSchema`, so the MCP input/output schemas carry the same descriptions.

## `zimz-mcp` (~700 lines incl. tests) and CLI

rmcp 3.4 (`server`, `transport-io`, `macros`), stdio only. Tools, all
`readOnlyHint`/`idempotentHint` true, `openWorldHint` false, each with an `outputSchema`:

| Tool | Notes |
|---|---|
| `list_archives` | optional substring `filter` |
| `search` | `query`, `archives`, `mode` (auto/fulltext/title/listing), `limit` ≤ 50, `cursor`, `snippet_chars`, `min_score`, `or_fallback` |
| `read_article` | `archive`, `path`, `format` (markdown/text/html), `max_chars` (8 000), `offset`, `section` |
| `outline`, `links` | heading tree; internal links with pagination |
| `suggest` | `prefix`, `archives`, `limit` |
| `context` | `query`, `archives`, `budget_chars` (12 000), `per_hit_chars` (1 500), `max_hits` (6) |
| `archive_health` | `archive`, `verify` (none/quick/full) |

Every result carries `structuredContent` (the typed response) and one text block rendered
for a model (ranked list with snippets, article with a `[showing characters … pick a
section: …]` trailer, packed excerpts with `Source:` lines). Library errors (unknown
archive, missing entry, empty query, bad cursor) are returned as tool errors (`isError`)
with the message, so the model can correct itself; only serialisation/worker failures are
protocol errors. Library calls run on `spawn_blocking`. Resources: `zim://{archive}`
(catalogue entry as JSON) and `zim://{archive}/{path}[?format=text|html]` (whole article).
`initialize` returns instructions that describe the intended workflow (`context` first,
`search` → `read_article`, cite `zim://` URIs). Logs go to stderr (`ZIMZ_LOG`/`RUST_LOG`).

CLI: `zimz mcp --zim-dir DIR [--zim FILE] [--no-recursive] [--cluster-cache-mb 256]
[--extract-cache-mb 64] [--priority GLOB=WEIGHT]`; `zimz archives --zim-dir DIR [--filter]
[--json]`; `zimz search <dir> "query" [--archive NAME] [--cursor] [--snippet-chars] [--json]`;
`zimz suggest <dir> prefix`; `zimz context <dir> "query" [--budget] [--per-hit] [--max-hits]`.
Single-file `search`/`suggest` keep their P2 behaviour.

Claude Code: `claude mcp add zimz -- zimz mcp --zim-dir ~/zims` (or the same command in
`.mcp.json`). See README.md.

## Verification

- `zimz-search`: 19 unit tests (fusion order, weights and boosts; cursor round trip and
  rejection; title-boost levels; percent scaling; character windows; excerpt choice; cache
  eviction by bytes; name/glob/priority helpers) and 17 integration tests over a temp
  directory of symlinked fixtures: old-scheme archive with both indexes, new-scheme
  archives with a title index only, an archive with no Xapian index (listing mode),
  duplicate `Name`s, a corrupt file, a non-ZIM file, a subdirectory (recursive and not).
  They check the catalogue, selection, exact-title-first ranking, snippet length and
  highlighting, stable pagination across cursors (two pages of 5 equal one page of 10),
  OR fallback ordering and flags, `min_score`, forced modes, federated suggestions, every
  path form accepted by `read_article`, redirects, sections, HTML/text formats, links
  resolution, context budget and citations, health with quick and full verification, and
  serde defaults.
- `zimz-mcp`: 7 end-to-end tests through a real rmcp client over an in-memory pipe:
  initialize (capabilities, instructions), the complete annotated tool list with a JSON
  snapshot of every schema (`tests/snapshots/tools.json`, refresh with
  `ZIMZ_UPDATE_SNAPSHOTS=1`), structured + text results for every tool, tool errors for bad
  archive/entry/query/arguments, resource listing, templates and reads.
- The `zimz mcp` binary driven with raw JSON-RPC (initialize → tools/list → context →
  search → read_article error → resources): all responses correct, 0.65 s for the whole
  session including the scan.

## Measured (M-series laptop, release, warm page cache, 51 archives / 181 GB in `~/zims`)

| Operation | Time |
|---|---|
| `Library::scan` of 51 archives (open + metadata + index probes, parallel) | 26–41 ms |
| Per-archive AND search, top 10, no snippets (worst: Wikipedia) | 0.6 ms |
| Federated search, 51 archives, no snippets | ≈ 1 ms |
| Federated search with 10 snippets of 300 chars | 18–230 ms cold, 9–120 ms warm (depends on article sizes) |
| `context`, 6 excerpts, 12 000-char budget | 13–156 ms |
| Federated `suggest`, 10 results | 130 ms (Wikipedia title index dominates, as in P2) |
| `zimz mcp` session: scan + initialize + 6 calls | 0.65 s |

Ranking spot checks after the strength term: "water purification methods" → Wikipedia
*Water purification*, WikEM *Water purification*; "quantum entanglement" → Wikipedia, then
two LibreTexts chapters; "kubernetes pod scheduling" → Wikipedia *Kubernetes*, *Pod*, a
ManKier page; "rust borrow checker" → Rust language pages. Before it, every archive's
rank-1 tied and a Bulbapedia page led two of these queries.

### Soak (`ZIMZ_TEST_ZIM_DIR=~/zims cargo test -p zimz-search --test local_library`)

Debug build with optimised dependencies, 10 rounds of 10 queries with 300-char snippets
over all 51 archives, then `context` and `suggest`; the test asserts that RSS growth stays
under the cache budgets plus mmap page-cache noise.

| | |
|---|---|
| scan | 51 archives, 0 failures, 30 ms |
| first-round searches | 51–470 ms, one outlier at 2.8 s ("kubernetes pod scheduling": cold ManKier/Gutenberg pages; 231 ms in release) |
| top hits | Photosynthesis, Carbon dioxide, Quantum entanglement, Causes of the French Revolution, Water purification (all Wikipedia), Kubernetes, ManKier's git-interactive-rebase-tool |
| `context` | 6 excerpts, 8 988 chars, 334 ms |
| `suggest` "carbon" | 10 suggestions, 123 ms |
| RSS | 562 MiB → 864 MiB after 100 searches (extract cache 15 MB / 64 MB budget, 911 hits / 111 misses; cluster caches ≤ 256 MiB) |

## Known limits

- RRF + strength is a heuristic; BM25 weights from different databases are only roughly
  comparable (IDF grows with collection size, so Wikipedia tends to win ties, which is
  usually right for encyclopaedic questions and wrong for narrow technical ones — use
  `archives` or `--priority` to steer).
- Listing mode (archives without any Xapian index) is a prefix scan of titles as typed
  and capitalised; no stemming, no body text.
- Snippets and context excerpts read at most 4 MiB of an item; a match deep inside a
  larger Gutenberg book gets a snippet from its beginning.
- `structuredContent` and the text block both go on the wire (spec recommendation), so a
  `read_article` response is roughly twice its `max_chars`.
- stdio only; no watch/reload when files change (restart the server).
- Single-word suggestions on Wikipedia remain 75–250 ms (P2 limit).
