# zimz — Plan

Open work only. Phases P0–P4 (core reader, glass reader, extraction, search + MCP), the
streamable HTTP transport and quoted-phrase search are done; their notes and measurements
are in `docs/`.

---

## 1. Out of scope

- Writing ZIM files.
- Serving kiwix-serve's HTTP API or UI (MCP + CLI only).
- Public internet exposure: the HTTP transport is for local and LAN use with a bearer
  token (no TLS, no OAuth; put a reverse proxy in front if needed).
- Video/image understanding.

---

## 2. Open items from finished phases

**Performance**
- Single-word title suggestions on Wikipedia take 75–250 ms against a 20 ms target: every
  candidate costs two position-list lookups and tie groups need title values. Candidates:
  skip the phrase checks when a one-word query cannot form a phrase, batch title reads, cap
  prefixes at two characters in the MCP layer.
- Early-stop cluster decode: stop decompressing once the requested blob is complete.
  zimz is 10–20 % slower than libzim on cold item reads (Wikipedia, iFixit) because it
  decodes the whole ~2 MiB cluster (`docs/bench-vs-python-libzim.md`).
- `madvise(RANDOM)` on index regions of large archives.

**Library**
- Lazy open plus a hot-archive LRU (`Library::scan` currently opens every archive up
  front).
- Directory watch and hot reload (`notify`); today new or changed files need a restart.

**Ranking**
- Small penalty for very short articles (tiny word counts) in the federation fusion.

**Extraction**
- Tables ignore `rowspan`/`colspan`.
- A Readability port for zimit pages if the generic main-container heuristics prove too
  weak.
- PDF and EPUB text (nautilus, Gutenberg), e.g. via `pdf-extract`.

**Testing and benchmarks**
- Run the `fuzz/` targets for 1 h (needs a nightly toolchain and `cargo-fuzz`).
- Tokenizer/normaliser golden tests generated from Xapian for en/fr/de/es and a CJK sample
  (today's analyzer tests are hand-written).
- criterion benchmarks for lookup/decode/query, and a script that runs the latency targets
  from `docs/P1-core-reader.md` and `docs/P2-glass-reader.md` against `~/zims` and writes
  `docs/bench-<date>.md`.
- Optional: spot-check ranking against kiwix-serve in Docker
  (`ghcr.io/kiwix/kiwix-tools`, `/search?format=xml`).

---

## 3. P5 — `zimz-index` built tier (tantivy) — deferred
Deferred: the embedded indexes cover every HTML page of the wiki-style archives. Worth
building when JSON-app ZIMs (LibreTexts, YouTube, nautilus, whose embedded full-text
indexes are near-empty) or real phrase/fuzzy/boolean search become a priority.

- Schema: `archive` (facet/str, fast), `path` (str, stored), `title` (text, boost 3,
  stored), `body` (text, not stored), `word_count` (u64 fast), `section_headings` (text).
  Tokenizer per archive language (Snowball + lowercase + ascii-fold; jieba/lindera for
  CJK), default fallback `simple`.
- One tantivy index **per ZIM** in `--index-dir/<uuid>/`, with a manifest (zim path, size,
  mtime, schema version). The builder streams front articles through the extraction
  adapters on a rayon pool; resumable in cluster order; memory budget flag; progress and
  ETA in the CLI.
- Query: tantivy `QueryParser` on `title, body` with phrase/fuzzy/boolean; snippets from
  the extraction cache, not from stored bodies (keeps indexes small).
- Tier selection per archive per query: built index if present and healthy, else embedded
  full text, else title-listing prefix scan; the `built` mode joins `search`'s `mode`.
  CLI: `zimz index build|status|rm`.
- Acceptance: devdocs set < 10 s total; wikispecies (3.4 GB) < 5 min; Wikipedia en build
  time and size recorded (budget ≈ 1–2 h and 8–12 GB on an M-series laptop); phrase query
  tests; RRF across mixed tiers; built-index query < 20 ms plus snippets.
- Risk: Wikipedia build cost. Mitigation: opt-in per archive, resumable, measured and
  documented; the glass tier already covers Wikipedia well.

---

## 4. Later, not scheduled

- Spelling correction (symspell over the title index terms) and query-time synonym
  expansion.
- Semantic/hybrid retrieval: planned as a separate system built on zimz's section-level
  `zim://` citations, not inside zimz.

---

## 5. Open risks

| Risk | Impact | Mitigation |
|---|---|---|
| Stemmer/tokenizer drift vs Xapian's Snowball snapshot → silent recall loss | Medium | Golden tests from Xapian (open item above); on zero postings for a stem, probe alternates (unstemmed, other Snowball revision); tier 2 as the escape hatch |
| MCP spec churn | Low | rmcp handles version negotiation; keep the tool surface small and schema-snapshotted |
| ZIM spec evolves (minor 4, new listings) | Low | Feature detection by dirent presence, not version numbers; follow testing-suite updates |
