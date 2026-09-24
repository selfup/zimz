# zimz — Plan

A native (no C++/libzim/Xapian) library + MCP server that reads ZIM files, searches a directory
full of them, and hands AI agents ranked, token-budgeted context. Target: replace the search
half of `kiwix-serve` and go beyond it.

Research date: 2026-09-22. Sources: openZIM wiki (spec), libzim 9.8.2 / libkiwix `main`,
xapian-core 1.4.31 / 2.1.0 sources, crates.io / pkg.go.dev, and byte-level inspection of the
57 ZIM files on this machine (`~/zims`, `~/Downloads`).

---

## 0. Summary and decisions

Decisions taken 2026-09-23 (details in section 10): **Rust**; **GPL-3.0-only, © Regis
Boudinot**; **MCP + CLI only** (no kiwix-serve HTTP shim); **Glass tier only for now** (the
tantivy tier stays designed but deferred); **stdio transport only for now**.

**Language: Rust.** (Section 5 has the matrix.) The two things that decide it are (a) tantivy
is 5–20x faster and 2–5x smaller than bleve when we build our own index, which matters for a
124 GB Wikipedia ZIM, and (b) both languages need the same hand-written Xapian "glass" reader,
so Go's simplicity buys nothing on the hardest component. Rust also gives a single static
binary with zero C++ and a current official MCP SDK (`rmcp` 3.4).

**Search architecture: two tiers behind one API.**

| Tier | Source | Setup cost | What it gives |
|---|---|---|---|
| 1. Embedded (default) | The Xapian glass DBs every Kiwix ZIM already carries (`X/fulltext/xapian`, `X/title/xapian`), read natively at a byte offset | none — instant on any directory | Exactly what kiwix-serve gives today: AND of stemmed terms, BM25, snippets, title suggestions |
| 2. Built (**deferred**) | Our own tantivy index built from extracted main-content text | minutes per GB of ZIM, ~0.3x text size on disk | Phrase, fuzzy, boolean, fielded title boost, clean snippets, unified cross-ZIM ranking, later hybrid/semantic |

A federation layer runs the best available tier per ZIM, fuses results across ZIMs (reciprocal
rank fusion + title-match boost + redirect de-duplication), and the MCP layer exposes
`list_archives`, `search`, `read_article`, `outline`, `suggest`, and a one-shot `context` tool
that returns search hits plus excerpts under a character budget.

**Why native instead of FFI:** every existing ZIM+MCP project either links libzim/Xapian
(GPL, C++ toolchain, no static binary) or proxies kiwix-serve over HTTP. No pure-Rust glass
reader exists; the only pure-Go attempt is an unfinished prototype. The embedded index is far
simpler than a general Xapian DB (no termlist, no positions, no term prefixes), so a read-only
reader is ~3k lines. That is the one genuinely novel piece of this project, and it is what
makes "point it at a folder and search 190 GB in milliseconds, no indexing" possible.

---

## 1. Goals and non-goals

Goals
- Read any ZIM produced since ~2017: spec major 5 and 6, minor 0–3, old (`A/`) and new (`C/`)
  namespace schemes, zstd and xz clusters, extended (>4 GiB) clusters, split `.zimaa` files.
- Full-text search and title suggestions with results equivalent to kiwix-serve, without
  building anything first.
- Cross-ZIM ranked search on top of the embedded indexes. Phrase/fuzzy/fielded queries
  come with the deferred tier 2.
- Article retrieval as Markdown/plain text with section addressing and pagination, tuned for
  LLM context windows.
- An MCP server over stdio for a ZIM directory, plus a CLI for humans and tests.
- Pure Rust, GPL-3.0-only, single binary, runs on macOS/Linux.

Non-goals (for now)
- Writing ZIM files.
- Serving kiwix-serve's HTTP API or UI. Decision: MCP + CLI only.
- Tier-2 built indexes (tantivy) and semantic/hybrid search: designed in 6.5 and section 7
  but deferred. Glass only for now.
- Streamable HTTP MCP transport: stdio only for now.
- Video/image understanding. PDF text extraction is optional later work.

---

## 2. The ZIM format (spec digest, verified against local files)

All integers little-endian. Offsets are absolute file offsets. Spec: https://wiki.openzim.org/wiki/ZIM_file_format

### 2.1 Header (80 bytes at offset 0)

| Off | Size | Field | Notes |
|---|---|---|---|
| 0 | 4 | magic | `0x044D495A` (72173914) |
| 4 | 2 | majorVersion | 5 = classic, 6 = "extended" (clusters may use 8-byte offsets). libzim accepts only 5 and 6 |
| 6 | 2 | minorVersion | 0..3. `>= 1` ⇒ new namespace scheme. Section 2.8 has what each value means |
| 8 | 16 | uuid | archive identity (used for cache keys, multi-ZIM dedup) |
| 24 | 4 | entryCount | number of dirents |
| 28 | 4 | clusterCount | |
| 32 | 8 | urlPtrPos | path pointer list |
| 40 | 8 | titlePtrPos | title pointer list; `0xFFFF_FFFF_FFFF_FFFF` = none (all local 6.3 files) |
| 48 | 8 | clusterPtrPos | cluster pointer list |
| 56 | 8 | mimeListPos | 80 (72 in ancient pre-checksum files; honour the field, never hardcode) |
| 64 | 4 | mainPage | entry index; `0xFFFFFFFF` = none |
| 68 | 4 | layoutPage | always `0xFFFFFFFF` (unused) |
| 72 | 8 | checksumPos | `= fileSize - 16` |

libzim sanity checks: `mimeListPos <= urlPtrPos`, `clusterCount <= entryCount`, counts both
zero or both non-zero.

### 2.2 MIME type list (at `mimeListPos`)
Consecutive NUL-terminated strings, terminated by an empty string. A dirent's `mimetype` u16
indexes this list. Values `>= 0xFFFD` are reserved (see 2.4). Observed lists include
`application/octet-stream+xapian` and `application/octet-stream+zimlisting`, the marker
mimetypes for indexes and listings.

### 2.3 Pointer lists
- **Path (URL) pointer list** at `urlPtrPos`: `entryCount` × u64 dirent offsets, sorted by
  `(namespace byte, path bytes)`. Path lookup is a binary search over this list (each probe
  reads one dirent). This is the primary index of the file.
- **Title pointer list** at `titlePtrPos`: `entryCount` × u32 entry indexes, sorted by
  `(namespace, title)`. In 5.x it is a standalone list; in 6.1/6.2 it points into the
  uncompressed cluster that holds `X/listing/titleOrdered/v0` (same bytes); in 6.3 it is absent.
- **Cluster pointer list** at `clusterPtrPos`: `clusterCount` × u64 cluster offsets. The
  format stores **no compressed length** and clusters need not be contiguous or in order
  (libzim #84), so a compressed cluster is decoded as a stream from `ptr[n] + 1` until the
  zstd/xz frame ends; never assume `ptr[n+1] - ptr[n]` bounds it. For uncompressed clusters
  the offset table gives the exact extent.

### 2.4 Directory entries (dirents)

| Off | Size | Field |
|---|---|---|
| 0 | 2 | mimetype index; `0xFFFF` = redirect, `0xFFFE` = linktarget, `0xFFFD` = deleted |
| 2 | 1 | parameter length (always 0 in practice) |
| 3 | 1 | namespace (ASCII byte) |
| 4 | 4 | revision (always 0) |
| 8 | 4 | content: cluster number / redirect: target entry index |
| 12 | 4 | content only: blob number |
| … | | `path\0`, `title\0`, `parameter[len]` |

Redirect dirents are 4 bytes shorter (no blob field). **Empty title means "title = path"**
(libzim `Dirent::getTitle`). Paths and titles can reach 64 KB in theory; read 2 KB and extend.
Redirect chains: follow with a hop limit (libzim uses 50). **Aliases** (6.2+): several dirents
may reference the same cluster/blob. Stored paths are raw UTF-8, **not** percent-encoded, so
HTTP/MCP request paths must be percent-decoded before lookup and fragments dropped. libzim's
path-resolution compatibility rules, worth copying: new scheme → look in `C/`, and if the
caller passed `X/foo` strip the namespace and retry; old scheme → the path carries its
namespace, otherwise try `A`, `I`, `J`, `-` in that order.

### 2.5 Clusters and blobs

Cluster at `ptr[n]`: one **uncompressed info byte** followed by the (possibly compressed)
payload.

- Info byte: low 4 bits = compression: `0`/`1` none, `2` zlib (obsolete, libzim rejects),
  `3` bzip2 (obsolete, rejects), `4` xz/LZMA2, `5` zstd. Bit `0x10` = **extended cluster**
  (8-byte offsets; only allowed when major = 6).
- Payload after decompression: an offset table `off[0..=n]` (u32, or u64 if extended),
  relative to the start of the table, followed by blob bytes. `n = off[0] / width - 1`
  blobs; blob `i` = `payload[off[i]..off[i+1]]`.
- Reading blob `i` therefore requires decompressing the cluster from the start through blob
  `i` (streaming decode can stop early). A **decompressed-cluster LRU cache** is mandatory;
  libzim keeps one too. Default cluster size written by libzim is ~2 MiB uncompressed.
- Validation (mirror libzim): `off[0]` is exactly `width` (zero blobs) or `>= 2·width`, a
  multiple of `width`; blob count `<= entryCount + 1`; offsets non-decreasing.
- Some 5.0 files were written with zstd level 22 and need a 128 MB decoder window; configure
  the decoder's max window size accordingly.
- Local library: only `1` (none) and `5` (zstd) observed; xz (`4`) appears in ZIMs from before
  ~2020 and must be supported (libzim's writer dropped xz in 8.0 but every pre-2021 ZIM uses
  it). No extended clusters observed, but support is required by spec.

### 2.6 Namespaces and well-known entries

Old scheme (minor 0; e.g. mwoffliner ≤ 1.11 ZIMs from 2021): `-` layout/assets, `A` articles,
`B` article meta, `I`/`J` images, `M` metadata, `U`/`V`/`W` categories, `X` indexes,
`Z//fulltextIndex/xapian` legacy index location. HTML lives at `A/<path>`; links are relative
(`../-/style.css`).

New scheme (minor ≥ 1, libzim ≥ 7, 2021-10): `C` all user content (HTML, CSS, images, JSON,
PDFs, everything), `M` metadata, `W` well-known (`W/mainPage` is a redirect to the main entry),
`X` indexes and listings. Paths under `C/` carry no `.html` suffix by convention
(`C/Zstd`, `C/api-trace2`, `C/www.mankier.com/1/cat` for zimit).

Main page: `header.mainPage` → dirent (in new-scheme files this is the `W/mainPage` redirect;
follow it). Old-scheme files point straight at `A/Main_Page`. `W/favicon` is an optional
redirect. Illustration: `M/Illustration_48x48@1`, falling back in old files to `-/favicon`,
`-/favicon.png`, `I/favicon`, `I/favicon.png`.

### 2.7 Metadata (`M/` namespace, one blob each, plain UTF-8)
`Name`, `Title`, `Creator`, `Publisher`, `Date` (YYYY-MM-DD), `Description`,
`LongDescription`, `Language` (ISO 639-3, comma-separated), `License`, `Tags`
(`;`-separated; conventions `_ftindex:yes|no`, `_pictures:`, `_videos:`, `_details:`,
`_category:<x>`), `Flavour`, `Source`, `Scraper`, `Counter` (`mime=count;…` over `C`
entries; mimetypes may themselves contain `;` and `=` parameters, so take the count after the
last `=` of each `;`-chunk and re-join chunks that lack one), `Illustration_48x48@1` (PNG),
`Relation`, custom `X-…` keys. Not all scrapers set all keys (several
local files lack `Tags` or `Scraper`). The `_ftindex` tag is advisory only — probe the file.

### 2.8 Minor versions and title listings (libzim ChangeLog + local verification)

| Version | Written by | Meaning |
|---|---|---|
| 5.0 | libzim ≤ 6.3 (until 2021; some scrapers later) | old namespaces; standalone title pointer list in the header |
| 6.0 | libzim 3.2–6.3, only when a cluster exceeded 4 GiB | **extended clusters** (8-byte blob offsets); still old namespaces |
| 6.1 | libzim 7.0 (2021-10) | **new namespace scheme** `C/M/W/X`, `W/mainPage`, `X/listing/titleOrdered/v0` + `v1`, `M/Counter`, zstd default; header `titlePtrPos` points *into* the uncompressed cluster that holds the v0 blob |
| 6.2 | libzim 9.1 (2023-12) | adds **alias** entries (dirents sharing one blob); otherwise as 6.1 |
| 6.3 | libzim 9.3 (2025-04) | **v0 title list removed**: header `titlePtrPos = u64::MAX`, only `v1` remains |

Local files: wikem 2021 = 5.0; zimgit/libretexts/ifixit 2024-25 = 6.2 (v0 + v1);
devdocs/wikipedia/gutenberg 2026 = 6.3 (v1 only). Kiwix still publishes 6.2 from python
scrapers on older libzim, and back catalogues hold 5.0/6.0/6.1, so all five must be readable.

`v0` = every entry sorted by `(namespace, title)`. `v1` = only **front articles** (writer
hint `FRONT_ARTICLE`, default = `text/html` items; zimit also flags PDFs; redirects included)
sorted by title (bytewise, shorter first; empty title ⇒ path). Both are u32 LE entry-index
arrays, mimetype `application/octet-stream+zimlisting`, **always in an uncompressed cluster**
(spec requirement for all indexes/listings). Wikipedia 2026-02: 27.2 M entries, ~19 M front
articles. libzim ≥ 9.3 reads only v1 and falls back to the header list.

Rule for the reader (libzim's own order): look up `X/listing/titleOrdered/v1` (must sit in an
uncompressed cluster) → else header `titlePtrPos` if `!= u64::MAX` → else "no title index".
Namespace scheme is `minor >= 1`. Never infer listing presence from the minor number alone.

### 2.9 Checksum and split files
- MD5 over bytes `[0, checksumPos)`, 16 raw bytes at `checksumPos`. Verify on demand only.
- Split archives: `foo.zimaa`, `foo.zimab`, … `zz` (≤ 676 parts, no gaps), produced by
  `zimsplit`, which cuts only at cluster boundaries (default 2 GiB parts). They are one
  logical file; all offsets are global. libzim refuses direct access (and thus Xapian) for a
  blob that straddles two parts; our glass reader takes a `BlockSource` (mmap slice, or
  `ReadAt` across parts) so tier 1 keeps working. Kiwix never publishes split files; users
  make them for FAT32 media.

### 2.10 How ZIMs are made (what shapes the content we'll extract)

Scrapers (openZIM org) → `python-scraperlib` / `node-libzim` → **libzim writer**
(`zim::writer::Creator`): assigns dirents, packs blobs into ~2 MiB clusters (zstd by default
today, xz before ~2020), stores indexes/listings uncompressed, builds the Xapian DBs
(section 3), sorts pointer lists, writes the MD5. `zimwriterfs` builds a ZIM from a directory;
`zim-tools` ships `zimdump`, `zimcheck`, `zimsearch`, `zimsplit`, `zimrecreate`.

Scraper families in the local library and how they store content:

| Scraper | Content storage | Extraction notes |
|---|---|---|
| mwoffliner 1.17 (wikipedia, wikispecies, proofwiki, bulbagarden, openzim) | Full HTML per article under `C/<Title>`; Vector skin markup | Main content = `.mw-parser-output`; strip `.navbox`, `.reflist`, `sup.reference`, `.mw-editsection`, hatnotes; infoboxes optional |
| mwoffliner 1.11 (wikem, AoPS; ZIM 5.0) | HTML under `A/<Title>`, mobile skin | Main content `#mw-content-text` |
| devdocs2zim | HTML under `C/<slug>` with a `<devdocs-navbar>` custom element | Main content `._content` / `._page` |
| warc2zim/zimit (mankier, cdc, devhints, jeffe) | Original site HTML under `C/<host>/<path>` with injected `wombat.js` replay glue; PDFs are front articles too | Readability-style extraction; strip injected scripts; PDF bodies need P6 |
| gutenberg2zim | Whole book HTML per entry (`C/<Title>.<id>`) plus EPUB/PDF | Drop `#pg-header`/`#pg-footer`; large documents (100 KB–MBs) need section paging |
| mindtouch2zim (libretexts) | Stub HTML at `C/index/page_<id>` (meta-refresh to a JS app); real body in `C/content/page_content_<id>.json` `{ "htmlBody": … }` | Adapter: map `index/page_N` → `content/page_content_N.json` |
| youtube2zim | Stub HTML at `C/index/<slug>`; metadata in `C/videos/<slug>.json` (title, description, chapters); subtitles as `.vtt` | Adapter: JSON + VTT → text |
| nautiluszim (zimgit-*) | A JS app over `database.js` and PDFs under `C/files/` | Titles/descriptions only; PDF text optional later |
| fcc2zim, ifixit2zim | HTML/JSON app bundles | Generic readability fallback |

Consequence: the Xapian fulltext index (built by libzim from the HTML it was given) is
near-empty for JSON-app ZIMs (libretexts, youtube, nautilus indexes are 20–50 KB regardless
of size), so tier 2 with adapters is the only way to search those well.

---

## 3. Search indexes inside a ZIM

### 3.1 Where they are and how libzim opens them

| Index | Path | Present in |
|---|---|---|
| Fulltext | `X/fulltext/xapian` (legacy: `Z//fulltextIndex/xapian`) | every local file (57/57); Kiwix catalog: 73 % tagged `_ftindex:yes`, but the tag is unreliable |
| Title | `X/title/xapian` | all but 3 local files (nautilus). Always built by libzim ≥ 4 when the writer has Xapian |

Both are **Xapian glass single-file databases** (`Xapian::WritableDatabase(..., DB_NO_TERMLIST)`
compacted with `DBCOMPACT_SINGLE_FILE | FULL`), stored as ordinary items in an
**uncompressed cluster**. libzim opens them with `lseek(fd, blobOffset)` +
`Xapian::Database(fd)`. With mmap this is just a byte slice of the ZIM. Verified: every
local index blob starts with `0F 0D "Xapian Glass" 04 6E` (format 2016-03-14, unchanged
through Xapian 2.1).

Sizes (local): Wikipedia en maxi 8.2 GB fulltext + 3.5 GB title; wikispecies 461 + 271 MB;
ifixit 79 + 29 MB; devdocs ≤ 2 MB each.

### 3.2 What the fulltext index contains (libzim writer `xapianIndexer.cpp`, `xapianWorker.cpp`)

- Documents: only `C/` items whose mimetype starts with `text/html` (or custom `IndexData`).
- HTML → text via `MyHtmlParser` (omindex-derived): skip `<script>/<style>`, keep `<body>`
  text, block tags become spaces, collect `<meta name=keywords>`, honour
  `<meta name=robots content=noindex>`; a literal `NOINDEX` in the text suppresses indexing.
- Normalisation: ICU transliterator `"Lower; NFD; [:M:] remove; NFC"` (lowercase, strip all
  combining marks) on content, title, keywords, and at query time.
- Tokeniser: Xapian `TermGenerator` with `FLAG_CJK_NGRAM` (CJK runs → unigrams + bigrams),
  Xapian word rules (letters/marks/digits/`Pc` are word chars; `'`, `&`, `·`, `‧`, `׳` infix;
  `, . ;` between digits; up to three trailing `+`/`#`; `U.N.`-style initials joined),
  max word length 64 bytes.
- Stemming: Snowball `Xapian::Stem(<2-letter code from Language>)`, strategy **`STEM_ALL`**:
  every term is stored **stemmed only, unprefixed** (no `Z` prefix, no unstemmed copy).
- Stopwords: `STOP_ALL`, but the list is looked up by the 3-letter language code against
  2-letter resource names, so it is empty. **Verified: no local index has a `stopwords`
  metadata entry** — stopwords are effectively never applied.
- **No positions** (`index_text_without_positions`) ⇒ phrase/NEAR impossible on this index.
  **No termlist table** ⇒ no per-document term enumeration.
- Weighting inputs: body wdf 1; title wdf `contentLength/500 + 1`; keywords wdf 3 (title and
  body share one term space — there are no fields).
- Per document: `data = "C/<path>"` (old scheme: `"A/<path>"`), value 0 = normalised title,
  value 1 = word count (decimal string), value 2 = serialised lat/long if the page had
  `<meta name="geo.position">`.
- DB metadata keys: `valuesmap = "title:0;wordcount:1;geo.position:2"`, `kind = "fulltext"`,
  `data = "fullPath"` (absent in 2021 files), `language = "eng"`.

### 3.3 What the title (suggestion) index contains
- One doc per front article **including redirects**, `data = "C/<path>"`, value 0 = original
  title, value 1 = redirect target path (for collapsing; `valuesmap = "title:0;targetPath:1"`;
  2021 files: `"title:0"`).
- Text indexed = `"0posanchor " + normalised title`, `STEM_SOME`, **with positions**, no
  stopper, max word 240. So the postlist has unstemmed terms (positional) plus `Z<stem>`
  terms (non-positional), and every title starts with the anchor term `0posanchor`.

### 3.4 Xapian glass single-file format — what a read-only reader needs

(Full field-level detail is in the research notes; xapian-core sources are unpacked under the
session scratchpad `src/` for reference. Summary of what matters.)

- **Version block** at offset 0 of the blob: 16-byte magic+version, 16-byte uuid, LEB128
  `revision`, six `RootInfo` records in order `POSTLIST, DOCDATA, TERMLIST, POSITION,
  SPELLING, SYNONYM` (`root` block, `level<<2|seq<<1|fake`, `num_entries`, `blocksize>>11`,
  `compress_min`, freelist string), then stats (`doccount`, `last_docid-doccount`,
  `doclen_lbound`, `wdf_ubound`, `doclen_ubound-wdf_ubound`, `oldest_changeset`,
  `total_doclen`, `spelling_wordfreq_ubound`). `root_is_fake` ⇒ empty table (TERMLIST in
  every ZIM index; POSITION in fulltext indexes).
- **Blocks**: block size 8192 (libzim never overrides); block `n` is at `blob + n*8192`;
  **all tables share one block-number space**, block 0 = version block. Block header
  (big-endian): u32 revision, u8 level, u16 max_free, u16 total_free, u16 dir_end, then a
  directory of u16 item offsets sorted by key.
- **Items**: leaf `u16 I` (bits: 0x80 compressed, 0x40 last component, 0x20 first
  component; size = (I & 0x1fff)+3), `u8 K`, key, optional u16 component number, tag chunk.
  Branch items: u32 child block, u8 K, key, u16 component. Tags may span several items and
  leaves; compressed tags are **raw deflate** (window bits −15) — only ever used for
  DOCDATA/TERMLIST in practice, never POSTLIST.
- **Varints**: LEB128 `pack_uint`; sort-preserving `pack_uint_preserving_sort` (length
  prefix in leading 1-bits); `pack_string_preserving_sort` (`\0` → `\0\xff`, `\0`
  terminator unless last).
- **POSTLIST keys/tags**: term postlist first chunk key = sort-packed term (no terminator);
  later chunks add the first docid. First-chunk tag: `termfreq, collfreq, first_did-1,
  is_last ('0'/'1'), last_did-first_did, wdf0, {did_delta-1, wdf}*`. Doc lengths are the
  special postlist with key `"\0\xe0"`. Metadata: key `"\0\xc0"+name`. Value stats
  `"\0\xd0"+slot`; value chunks `"\0\xd8"+slot+first_did` with `{did_delta-1, string}`.
  Special keys sort before all terms.
- **DOCDATA**: key = sort-packed docid, tag = data string (`C/<path>`), deflated if > 18 B.
- **POSITION** (title index only): key = term + docid; tag = last position, then an
  interpolative-coded bitstream of the rest. Needed only for phrase/anchored suggestion
  queries.
- **BM25** (Xapian defaults `k1=1, k2=0, k3=1, b=0.5, min_normlen=0.5`):
  `idf = ln((N - tf + 0.5)/(tf + 0.5))` with the `tw < 2 ⇒ tw*0.5+1` tweak; term weight
  `× (k3+1)·wqf/(k3+wqf) × (k1+1)`; doc part `wdf / (k1·(b·normlen + 1−b) + wdf)` where
  `normlen = max(doclen/avglen, 0.5)`; `avglen = total_doclen/doccount` from the stats.
- Estimated size: ~2.5–3.5k LOC (header 100, varints 150, B-tree cursor 450, postlist 250,
  doclen/stats/values/metadata 200, docdata 30, AND/OR + BM25 + top-k 350, tokenizer +
  normaliser + stemmer glue 300, snippet 200) + ~300 for position lists.

### 3.5 How libzim executes a fulltext search (`search.cpp`)
- `Searcher(archives)` adds each archive's DB to one `Xapian::Database`; docid → archive by
  `(docid-1) % n`. **Metadata (language, stemmer) from the first archive only.**
- `QueryParser`: default op **AND**, same stemmer, `STEM_ALL`, parse with **only**
  `FLAG_CJK_NGRAM` — no boolean syntax, no quotes, no wildcards, no `+/-`. The query is
  normalised, tokenised, stemmed, ANDed.
- Enquire with default BM25, `get_mset(start, max)`; estimated total via `get_mset(0,0,10)`.
- Per result: path (strip `C/`), real dirent title, `percent` score, word count (value 1),
  snippet = re-read the article HTML, `MyHtmlParser` dump, `MSet::snippet(text, 500,
  stemmer)` with `<b>` markup. Snippets dominate latency (article decompression per hit).
- Geo: optional `LatLongDistancePostingSource` filter on value 2.

### 3.6 Suggestions (`suggestion.cpp`)
- Single archive only (multi-ZIM suggestions are an open libzim issue).
- Query = `parse(FLAG_DEFAULT|FLAG_PARTIAL|FLAG_CJK_NGRAM, STEM_SOME)` OR `OP_PHRASE(terms)`
  OR `OP_PHRASE("0posanchor" + terms)`; weighting `BM25(k1=0.001, b=1)` so short titles win;
  sort by relevance then title; **collapse on `targetPath`** so a redirect and its target
  appear once. Result = title, path, title with `<b>` highlights.
- Fallback without a title index: byte-wise, case-sensitive prefix binary search on the
  title listing (`findByTitle`).

### 3.7 kiwix-serve HTTP surface (what "replace the search functionality" means concretely)
- `GET /search?pattern=&books.name=…|books.id=…|content=…|books.filter.{lang,category,tag,…}&start=0&pageLength=25(max 140)&format=xml|html[&latitude&longitude&distance]`
  → OpenSearch RSS (`<opensearch:totalResults>`, per `<item>`: title, link
  `/content/<book>/<path>`, `<description>` snippet with `<b>`, `<book><title>`,
  `<wordCount>`). Errors: 400 for no query / unknown book / **books in different languages
  ("confusion of tongues")** / over `--searchLimit`; 404 "Fulltext search unavailable".
- `GET /suggest?content=<book>&term=<text>&count=10&start=0` → JSON
  `[{value,label,kind:"path",path}, …, {value:"<term> ",label:"containing '…'",kind:"pattern"}]`.
- `GET /search/searchdescription.xml`; `/content/<book>/<path>`; `/raw/<book>/content/<path>`;
  `/raw/<book>/meta/<Name>`; `/random?content=`; `/catalog/v2/{root.xml,entries,partial_entries,entry/<uuid>,categories,languages,illustration/<uuid>}`.
- Caches: searcher per book-set, `KIWIX_SEARCH_CACHE_SIZE` (default 2) parsed searches,
  suggestion searcher per book.

### 3.8 Known kiwix search weaknesses we should fix, not copy
- One stemmer/language per DB; multi-language libraries refused outright (libzim #734,
  libkiwix #1159).
- No phrase/boolean/wildcard in fulltext; no spelling correction (libzim #794, #731).
- Ranking: exact-title matches buried (libzim #766, #458); no popularity signal (#653);
  navigation boilerplate is indexed (#952).
- Snippet generation is the slow path (libkiwix #395); RPi searches of 40 s (#345).
- Suggestions can't span ZIMs (#932). CJK n-grams only in indexes built after 2023-07.
- French elision tokens (`d'actium`) don't match `actium` (#592).

---

## 4. The local library (ground truth for tests and sizing)

51 files / ~181 GB in `~/zims`, 6 more (~6 GB) in `~/Downloads`. Spec versions 5.0, 6.2,
6.3. All zstd or uncompressed clusters. All carry `X/fulltext/xapian`; 54 carry
`X/title/xapian`; all Xapian blobs are in uncompressed clusters.

| File | Size | ZIM | Entries | Scraper | FT idx | Title idx |
|---|---|---|---|---|---|---|
| wikipedia_en_all_maxi_2026-02 | 124 GB | 6.3 | 27.2 M | mwoffliner 1.17.5 | 8.2 GB | 3.5 GB |
| gutenberg_en_lcc-q_2026-03 | 17.7 GB | 6.3 | 150 k | gutenberg2zim 3.0.1 | 32 MB | 1.2 MB |
| ifixit_en_all_2025-12 | 3.6 GB | 6.2 | 895 k | ifixit2zim | 79 MB | 29 MB |
| wikispecies_en_all_maxi_2026-04 | 3.4 GB | 6.3 | 1.81 M | mwoffliner 1.17.5 | 461 MB | 271 MB |
| bulbagarden_en_all_maxi_2026-05 | 3.0 GB | 6.3 | 439 k | mwoffliner 1.17.5 | 60 MB | 35 MB |
| libretexts.org_en_chem_2025-01 | 2.2 GB | 6.2 | 297 k | mindtouch2zim 0.1.1 | 64 MB | 19 MB |
| www.mankier.com_en_all_2026-04 | 0.19 GB | 6.3 | 73 k | warc2zim 2.3.0 | 56 MB | 36 MB |
| proofwiki_en_all_maxi_2026-04 | 0.10 GB | 6.3 | 80 k | mwoffliner 1.17.5 | 22 MB | 23 MB |
| wikem_en_all_maxi_2021-02 | 0.04 GB | **5.0** | 8 k | mwoffliner 1.11.3 (old `A/`) | 6.8 MB | 1.2 MB |
| devdocs_en_* (22 files) | < 30 MB | 6.3 | 20–5 k | devdocs2zim 0.2.1 | ≤ 2 MB | ≤ 1.3 MB |
| zimgit-* (3, nautilus) | ≤ 0.6 GB | 6.2 | < 1 k | nautiluszim 1.1.1 | 20 KB (empty) | none |
| youtube2zim (3) | ≤ 2.9 GB | 6.3 | < 1 k | youtube2zim 3.5.0 | ~0.1 MB | ~0.1 MB |

Test corpus tiers: **tiny** (devdocs, openzim, zimgit — seconds), **medium** (wikem for old
scheme, ifixit/libretexts for 6.2 + v0/v1 listings, mankier for zimit), **large**
(wikispecies), **stress** (wikipedia). Never load the stress tier in unit tests.

---

## 5. Ecosystem survey and language decision

Rust
- ZIM: `zim` 0.5.0 (dignifiedquire; Apache/MIT; spec-tested against the openZIM testing
  suite; split files; ZIM 6.3) — fork/reference for our reader (~2k LOC). `libzim-rs`
  (GPL-3, zstd only), `zim-rs` (dead FFI) — avoid. `jasontitus/zimru` (MIT, unpublished) and
  `kohlhofer/offline-knowledge` (MIT, tantivy + rmcp, Sept 2026) — design references.
- Search: **tantivy 0.26** (BM25 + block-max WAND, phrase/fuzzy/regex, `SnippetGenerator`,
  18 Snowball stemmers, `tantivy-jieba`/`lindera-tantivy` for CJK; ~85 MB/s indexing,
  index ≈ 0.2–0.4× extracted text without stored bodies).
- Codecs: `zstd` 0.14 (C, fastest) or `ruzstd` 0.9 (pure, ~3.5× slower); `liblzma` 0.4 (C)
  or `lzma-rust2` 0.21 (pure, fast). Feature-flag both.
- HTML: `lol_html` (streaming, for indexing), `scraper`/`html5ever` (DOM pruning),
  `htmd` / `fast_html2md` (Markdown), `dom_smoothie` (Readability port).
- Xapian parity: `rust-stemmers` (Snowball), `unicode-normalization`, `flate2`.
- MCP: **`rmcp` 3.4.0** (official; spec 2026-07-28 with back-compat; stdio + streamable
  HTTP; `#[tool_router]` macros; `outputSchema`/`structuredContent`).
- Embeddings (later): `fastembed` 7 (ONNX), `usearch`/`hnsw_rs`.

Go
- ZIM: `justinstimatze/gozim` (MIT, zstd only, bleve, official go-sdk MCP already),
  `cookiengineer/gozim` (MIT, xz+zstd+multipart, own codecs, **unfinished glass reader**
  with real bugs: zlib instead of raw deflate, guessed table offsets), `tamnd/kage/zim`
  (zstd, new scheme only). `akhenakh/gozim` unmaintained since 2021.
- Search: bleve 2.6 (BM25 optional; documented 5–20× slower indexing and 2–5× larger
  indexes than tantivy; scorch bloat issues), bluge dead.
- Codecs: `klauspost/compress` zstd (excellent, pure), `ulikunitz/xz` (pure, ~2.5× slower
  than liblzma).
- MCP: official `go-sdk` v1.8 (current spec); `mark3labs/mcp-go` lags.
- HTML: `x/net/html`, `readeck/go-readability/v2`, `html-to-markdown/v2`.

Decision matrix

| Criterion | Rust | Go |
|---|---|---|
| Own full-text index at Wikipedia scale | tantivy: ~1 h, ~8–12 GB | bleve: many hours, 20+ GB |
| Glass reader | write it (~3k LOC) | write it (~3k LOC); prototype exists but is wrong |
| Static single binary, no C++ | yes (pure codecs available) | yes |
| MCP SDK currency | rmcp 3.4 ✔ | go-sdk 1.8 ✔ |
| HTML → Markdown quality | good (htmd/fast_html2md) | good (html-to-markdown v2) |
| Memory control for 8 GB index blobs | mmap slices, zero-copy | mmap ok, GC pressure on hot paths |
| Existing reference reader | `zim` 0.5 (spec-tested) | several, none complete |

**Rust, confirmed 2026-09-23.** (For the record: a Go port would keep the architecture and
swap tantivy → bleve, rmcp → go-sdk, under the CLAUDE.md Go rules.) With GPL-3.0 chosen,
`zimz-glass` may port Xapian's own glass code (GPL-2.0-or-later, compatible) rather than
clean-room it, which lowers the risk on the hardest component.

Existing ZIM+MCP servers (openzim-mcp, roanpy/kiwix-mcp, gozimmcp, zim-mcp, and three
kiwix-serve HTTP proxies) inform the tool surface (section 6.7) but none is native +
cross-ZIM + snippet-capable in one binary.

---

## 6. Architecture

### 6.1 Workspace layout

```
zimz/
  Cargo.toml                # workspace; edition 2024; deny(warnings) in CI
  PLAN.md
  crates/
    zimz-core/              # ZIM reader (no search)
    zimz-glass/             # Xapian glass single-file reader + query engine
    zimz-extract/           # HTML/JSON → text & Markdown, scraper adapters, snippets
    zimz-index/             # tier 2 (deferred): tantivy index build + query
    zimz-search/            # library scan, catalog, tier selection, federation, ranking
    zimz-mcp/               # MCP server (rmcp) over stdio
    zimz-cli/               # `zimz` binary: info/ls/cat/search/suggest/extract/mcp/check
  LICENSE                   # GPL-3.0-only
  fixtures/                 # openZIM testing-suite files (git submodule or downloaded)
  tests/                    # cross-crate integration tests gated on ZIMZ_TEST_ZIM_DIR
```

Dev loop per CLAUDE.md: `cargo fmt`, `cargo check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`, then `cargo run`. Zero warnings.

### 6.2 `zimz-core` — ZIM reader
- `Archive::open(path)` → mmap (memmap2) of the file or of a `Parts` list for split archives;
  parse header; lazy MIME list; detect scheme/listings as in 2.8.
- Zero-copy dirent parsing from the mmap; `Entry { index, namespace, path, title,
  kind: Item{cluster, blob, mime} | Redirect{target} }`. Redirect resolution with a loop
  guard.
- Lookups: `entry_by_path(ns, path)` binary search; `entry_by_index`; `title_range(prefix)`
  over the best available title listing (header list → v0 → v1) + a `front_articles()`
  iterator; `main_entry()`; `metadata(name)`; `iter_namespace(ns)`.
- Clusters: `Cluster::read(n)` → decoded `Bytes` with offset table; codec by info byte
  (none / zstd / xz; zlib/bzip2 → typed error); extended offsets; decoders run as streams
  from `offset + 1` with no assumed compressed length (2.3); **LRU cache by cluster number
  with a byte budget** (default 256 MB, configurable); optional early-stop decode for large
  clusters when only a low blob is wanted.
- `Blob` API: `item.bytes()` (Arc-backed slice into the cached cluster), `direct_range()`
  for blobs in uncompressed clusters (this is what the glass reader uses to get a byte
  slice of the Xapian DB without copying). `Source` trait = `ReadAt + mmap slice`, with a
  `Parts` implementation for split archives.
- `verify_checksum()`; `Illustration`; `Counter` parsing.
- Errors: typed (`ZimError::{Io, BadMagic, UnsupportedVersion, UnsupportedCompression,
  Corrupt(&'static str)}`), never panic on malformed input (fuzz target).

### 6.3 `zimz-glass` — embedded Xapian reader
- `GlassDb::open(impl BlockSource)` — a mmap slice normally, `ReadAt` for split archives.
  Parses version block and stats; per-table
  `BTree { root, level }` with a block accessor `block(n) -> &[u8]` (pure slice math on
  the mmap; the OS page cache is the block cache; add a small decoded-leaf cache later if
  profiling says so).
- `Cursor`: `seek_le(key)`, `seek_ge(key)`, `next()`, `key()`, `tag()` (chunk join,
  optional raw inflate).
- `PostList` iterator per term: `termfreq`, `collfreq`, `next() -> (docid, wdf)`,
  `skip_to(docid)` across chunks. `DocLengths` (the `\0\xe0` list) with `skip_to`.
- `metadata(key)`, `value(docid, slot)`, `docdata(docid)`, `valuesmap()`.
- `PositionList` (title index only).
- Query engine: `Analyzer { normalise (lower → NFD → drop marks → NFC), xapian_tokenize
  (with CJK n-gram), stem (rust-stemmers by 2-letter code, none if unsupported) }`,
  `Query::and(terms) / or(terms) / phrase(terms)`, doc-at-a-time evaluation with BM25
  exactly as 3.4, top-k heap, `percent` scaling like Xapian (100 × w/maxw).
- `SuggestionIndex`: STEM_SOME analysis, last-term prefix expansion by cursor scan,
  anchored/ordered phrase via positions, `BM25(k1=0.001,b=1)`, collapse by `targetPath`.
- Parity oracle: python-libzim via `uv run scripts/parity.py` (the wheels bundle libzim+Xapian;
  deps live in `pyproject.toml`/`uv.lock`) and `brew install xapian` (`xapian-delve`, `quest`)
  on blobs extracted with `zimz dump-index`.
  Term-level tests (postlist for term X equals `xapian-delve -t`), ranking tests (top-k
  vs `zim::Searcher`), and tokenizer golden tests generated from Xapian.
- Explicit scope guard: no write support, no honey backend, no remote DBs, no spelling/synonym
  tables.

### 6.4 `zimz-extract` — content to text/Markdown
- Two outputs: **index text** (fast, streaming `lol_html`, boilerplate-stripped, used by
  tier 2 and by snippet generation) and **agent Markdown** (DOM prune with `scraper`, then
  `htmd`; tables kept; images as `![alt]`; internal links rewritten to `zim://<archive>/<path>`).
- `Adapter` trait selected by `M/Scraper` + path heuristics: `MwOffliner`, `DevDocs`,
  `Zimit` (readability + strip wombat), `Gutenberg`, `LibreTexts` (stub → JSON `htmlBody`),
  `YouTube` (JSON + VTT), `Generic` (readability → body). Each adapter yields
  `Document { title, sections: [(heading_path, text)], links, word_count }`.
- `Outline` = heading tree with byte/char ranges so `read_article(section=…)` can slice.
- `Snippet::best_window(text, query_terms, len)`: Xapian-like windowed term-density scoring,
  `<b>`/`**` highlighting of stems matched via the same analyzer.
- Extraction cache: LRU keyed by `(uuid, entry index)` for hot articles.

### 6.5 `zimz-index` — built tier (tantivy) — deferred
Deferred by decision (Glass only for now). Kept as the design for when JSON-app ZIMs or
phrase search become a priority.
- Schema: `archive` (facet/str, fast), `path` (str, stored), `title` (text, boost 3, stored),
  `body` (text, not stored), `word_count` (u64 fast), `section_headings` (text). Tokenizer per
  archive language (Snowball + lowercase + ascii-fold; jieba/lindera for CJK), default
  fallback `simple`.
- One tantivy index **per ZIM**, in `--index-dir/<uuid>/`, with a manifest (zim path, size,
  mtime, schema version). Builder streams front articles through the adapter with a
  rayon pool; resumable in cluster order; memory budget flag. Progress + ETA in CLI.
- Query: tantivy `QueryParser` on `title, body` with phrase/fuzzy/boolean; snippet from
  the extraction cache, not from stored bodies (keeps indexes small).
- Sizing targets: devdocs set < 10 s total; wikispecies (3.4 GB) < 5 min; Wikipedia en:
  budget ≈ 1–2 h on an M-series laptop and 8–12 GB disk (estimate; measure and record).

### 6.6 `zimz-search` — library, tiers, federation
- `Library::scan(dirs, recursive)` → `Catalog { archives: [ArchiveInfo { name (from
  `M/Name` or filename), uuid, title, description, language(s), date, tags, category,
  size, entry_count, front_article_count, has_ft, has_title, built_index: Option<…> }] }`.
  Optional `--watch` (notify) to pick up new files. Lazy open; hot archive LRU.
- Tier selection per archive per query: built index if present and healthy, else embedded
  fulltext, else title-listing prefix scan. Reported back in every result (`mode`).
- Federation: fan out (rayon) to selected archives with per-archive `k`, then
  **reciprocal rank fusion** (k=60) across archives (raw BM25/percent scores are not
  comparable across DBs), plus boosts: exact/prefix title match (from the title index or
  listing), archive priority weights from config, and small penalty for tiny word counts.
  De-duplicate by resolving redirects to their targets. Stable pagination via cursor
  = (query hash, offset).
- Query semantics presented to agents: default AND-of-terms; automatic **OR fallback**
  when AND yields < N hits (flagged in the response); quoted phrases honoured on tier 2
  and approximated by AND + snippet proximity on tier 1; language auto-detect of the query
  is out of scope — use each archive's language.
- Multi-language libraries are allowed (unlike kiwix-serve): each archive is analysed with
  its own stemmer.

### 6.7 `zimz-mcp` — tools, resources, transports
Transport: stdio only for now; rmcp's streamable HTTP can be added behind a flag later
without changing the tool surface. All tools
`readOnlyHint: true`, `idempotentHint: true`, with `outputSchema` + `structuredContent`
and a text rendering for older clients.

| Tool | Input | Output |
|---|---|---|
| `list_archives` | `filter?` (name/tag/lang/category substring) | archives with name, title, description, language, date, size, entry counts, index availability, `mode` |
| `search` | `query`, `archives?` (names/ids/glob), `mode?` (auto\|fulltext\|title\|built), `limit` (≤50, default 10), `cursor?`, `snippet_chars` (default 300), `min_score?` | hits `[ {archive, path, title, score, mode, snippet, word_count} ]`, `total_estimate`, `next_cursor`, `fallback_used` |
| `read_article` | `archive`, `path`, `format?` (markdown\|text\|html), `max_chars` (default 8 000), `offset?`, `section?` (heading text or index) | `title`, `content`, `truncated`, `next_offset`, `word_count`, `outline` (headings only if `max_chars` exceeded), `links_count` |
| `outline` | `archive`, `path` | heading tree with char sizes per section |
| `suggest` | `prefix`, `archives?`, `limit` | `[ {archive, path, title} ]` (title index / listing) |
| `context` | `query`, `archives?`, `budget_chars` (default 12 000), `per_hit_chars` (default 1 500), `max_hits` (default 6) | one call: search + fused ranking + best-section excerpt per hit, packed under the budget, with source citations `zim://archive/path#section` |
| `links` | `archive`, `path`, `direction=out` | internal links (titles + paths), for agent browsing |
| `archive_health` | `archive?` | checksum status, index status, cache stats |

Resources: template `zim://{archive}/{path}` (Markdown by default; `?format=html`), and
`zim://{archive}` for metadata. Server config: `--zim-dir` (repeatable, recursive),
`--index-dir`, `--cluster-cache-mb`, `--extract-cache-mb`, `--priority archive=weight`.

Token economy rules baked in: snippets ≤ `snippet_chars`; `read_article` never returns more
than `max_chars`; oversize answers include the outline so the agent can pick a section;
`context` always cites `archive + path` so the agent can `read_article` for more.

### 6.8 `zimz-cli`
`zimz info <zim>` (header, metadata, listings, index sizes), `zimz ls <zim> [--ns C]`,
`zimz cat <zim> <path> [--md]`, `zimz search <dir|zim> "<query>" [--mode] [--json]`,
`zimz suggest`, `zimz dump-index <zim> --kind fulltext -o blob.glass` (for oracles),
`zimz mcp`, `zimz check <zim>` (MD5 + structure). `zimz index build|status|rm` arrives with
the deferred tier 2. No HTTP server.

### 6.9 Performance targets (M-series laptop, warm cache unless noted)

| Operation | Target |
|---|---|
| Open 124 GB ZIM | < 50 ms (header + MIME list only) |
| Path lookup | < 50 µs (≈25 dirent probes) |
| Blob read, cached cluster | < 100 µs; uncached zstd cluster ~2 MiB: < 5 ms |
| Embedded FT query, 3 terms, top-10 without snippets, Wikipedia | < 50 ms warm, < 1 s cold |
| Same with snippets | < 300 ms (10 cluster reads + extraction) |
| Suggestion prefix, Wikipedia title index | < 20 ms |
| `context` over all 57 archives | < 1.5 s |
| Built-index query (deferred tier) | < 20 ms + snippets |
| Memory | mmap only; RSS bounded by cluster/extract caches (default ≈ 400 MB) |

---

## 7. Phased roadmap

Each phase ends with `cargo clippy -D warnings` clean, tests green, and a short
`docs/<phase>.md` note of measured numbers.

**P0 — Bootstrap (S)**
Workspace, CI (fmt/clippy/test), fixtures from `openzim/zim-testing-suite`,
`ZIMZ_TEST_ZIM_DIR` gating for local-library tests, `cargo fuzz` scaffolding, `LICENSE`
(GPL-3.0-only) and SPDX headers.

**P1 — `zimz-core` reader (M)**
Everything in 6.2. Acceptance: passes all testing-suite fixtures (incl. corrupted ones
without panics); byte-identical blobs vs python-libzim for 1 000 random entries in each
local tier-tiny/medium file plus 200 in Wikipedia; `zimz info/ls/cat` work on all 57 files;
old-scheme (wikem) and split-file tests; fuzz 1 h clean.

**P2 — `zimz-glass` (L, highest risk — start with a 2-day spike)**
Spike: parse version block + walk the postlist for one term on devdocs_git, compare to
`xapian-delve`. Then full reader + engine per 6.3, fulltext first, then title index with
positions. Acceptance: on 10 archives × 50 queries (drawn from titles, random body terms,
2–4-term AND), top-10 set overlap ≥ 95 % and identical top-1 in ≥ 90 % vs python-libzim
`Searcher`; suggestion top-5 overlap ≥ 90 % vs `SuggestionSearcher`; Wikipedia query
latency within 6.9; tokenizer golden tests pass for en/fr/de/es + a CJK sample.

**P3 — `zimz-extract` (M)**
Adapters and both outputs per 6.4; snippet generator; outline. Acceptance: golden Markdown
for 3 pages per scraper family; boilerplate ratio measured (< 5 % nav text in Wikipedia
output); libretexts/youtube adapters return real text.

**P4 — `zimz-search` + `zimz-mcp` v1 (M)**
Catalog, tier 1 federation, RRF, MCP tools and resources (6.7) on stdio; `zimz search`,
`zimz mcp`. Acceptance: Claude Code configured with the server can answer questions from
devdocs + Wikipedia via `context`; latencies in 6.9; 100-query soak with no unbounded
memory growth.

**P5 — `zimz-index` tier 2 (M) — deferred**
tantivy build/query per 6.5, auto tier selection, `zimz index`. Acceptance: devdocs set
< 10 s; wikispecies < 5 min; recorded Wikipedia build time/size; phrase query tests; RRF
across mixed tiers.

**Later, not scheduled**
- Streamable HTTP MCP transport (with a bearer token) for remote agents.
- Hybrid ranking: `fastembed` (e.g. bge-small) over extracted sections + `usearch`, fused
  with BM25 by RRF; opt-in per archive because of build cost.
- Spelling correction (symspell over the title index terms), query-time synonym expansion.
- PDF text (nautilus, gutenberg) via `pdf-extract` at index build time.
- Directory watch + hot reload.
- A kiwix-serve-compatible HTTP shim was considered and rejected (MCP + CLI only).

Rough size for the scheduled phases: core 2.5k, glass 3.5k, extract 2k, search 1.5k,
mcp 1k, cli 0.8k ≈ 11k LOC plus tests (tier 2 would add ~1.5k).

---

## 8. Testing and parity strategy

- **Fixtures**: `openzim/zim-testing-suite` (small.zim, old-scheme and new-scheme samples,
  corrupted headers). Vendored via submodule or downloaded by a script into `fixtures/`.
- **Oracles**: python-libzim (`uv run scripts/parity.py`, deps in `pyproject.toml`) for entries, search and suggestions;
  Xapian CLI (`brew install xapian`: `xapian-delve`, `quest`, `xapian-check`) on index blobs
  written out by `zimz dump-index`; optionally kiwix-serve in Docker
  (`ghcr.io/kiwix/kiwix-tools`) to spot-check ranking against `/search?format=xml`.
- **Property/fuzz**: header/dirent/cluster parsers and the glass B-tree cursor under
  `cargo fuzz`; proptest for varint codecs (round-trip against reference encoders).
- **Golden files**: tokenizer/normaliser outputs generated from Xapian; Markdown outputs per
  scraper; MCP tool JSON schemas snapshot-tested.
- **Benchmarks**: criterion for lookup/decode/query; a `bench/` script that runs the 6.9
  table against `~/zims` and writes `docs/bench-<date>.md`. `scripts/bench_compare.py`
  compares against python-libzim on a shared workload (see `docs/bench-vs-python-libzim.md`).
- **Local-library integration tests** are gated on `ZIMZ_TEST_ZIM_DIR` and tiered (tiny in
  CI-like runs, large/stress only on demand).

---

## 9. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Stemmer/tokenizer drift vs Xapian's Snowball snapshot → silent recall loss on the embedded index | Medium | Golden tests from Xapian; on zero postings for a stem, probe alternates (unstemmed, other Snowball revision); keep tier 2 as the escape hatch |
| Glass edge cases (multi-leaf tags, deflated docdata, very large postlists, branch-key truncation) | Medium | Test on Wikipedia's 8 GB index early (P2 spike); fuzz the cursor; compare `xapian-check` stats |
| Unicode normalisation differences vs ICU (`Lower; NFD; [:M:] remove; NFC`) | Low | `unicode-normalization` + mark filtering; golden tests with accented/CJK/Turkish samples |
| Embedded index is useless for JSON-app ZIMs | Known | Adapters + tier 2; `list_archives` reports index quality (doc count vs front articles) |
| Wikipedia tier-2 build cost (time/disk) | Medium | Opt-in; resumable; measure and document; tier 1 already covers Wikipedia well |
| mmap of a 124 GB file on macOS + many archives open | Low | Lazy open, hot-archive LRU, `madvise(RANDOM)` for index regions |
| MCP spec churn (2026-07-28 stateless core) | Low | rmcp handles version negotiation; keep tool surface small and schema-versioned |
| Licensing of ported code | Low | zimz is GPL-3.0-only; Xapian and libzim are GPL-2.0-or-later, so porting their glass/format code is compatible. Keep provenance notes and SPDX headers in `zimz-glass` |
| ZIM spec evolves (minor 4, new listings) | Low | Feature detection by dirent presence, not version numbers; testing-suite updates |

---

## 10. Decisions (2026-09-23)

| Question | Decision |
|---|---|
| Language | **Rust** |
| License | **GPL-3.0-only**, copyright Regis Boudinot (`LICENSE` at repo root) |
| Surface | **MCP + CLI only**; no kiwix-serve-compatible HTTP endpoint |
| Search tiers | **Glass (embedded Xapian) only for now**; tier 2 (tantivy) and semantic search deferred |
| MCP transport | **stdio only for now**; streamable HTTP later if needed |

Scheduled phases are therefore P0–P4 (bootstrap, core reader, glass reader, extraction,
search + MCP). P5 and the "later" list stay in the plan as designed but unscheduled work.

Status: P0–P1 done (`docs/P1-core-reader.md`), P2 done (`docs/P2-glass-reader.md`),
P3 done (`docs/P3-extract.md`), P4 done (`docs/P4-search-mcp.md`).

---

## 11. References

Spec and openZIM (the live wiki sits behind an anti-bot challenge as of 2026-09; the
Wayback Machine has readable snapshots from late 2025)
- ZIM file format: https://wiki.openzim.org/wiki/ZIM_file_format
- Old namespace scheme: https://wiki.openzim.org/wiki/ZIM_file_format_old_namespace
- Article format (link rules): https://wiki.openzim.org/wiki/Article_Format
- Annotated example file: https://wiki.openzim.org/wiki/ZIM_File_Example
- Search indexes: https://wiki.openzim.org/wiki/Search_indexes
- Metadata: https://wiki.openzim.org/wiki/Metadata
- Naming convention: https://wiki.openzim.org/wiki/ZIM_Naming_Convention
- zim-tools (zimdump, zimcheck, zimsplit, zimwriterfs): https://github.com/openzim/zim-tools
- Testing suite: https://github.com/openzim/zim-testing-suite
- libzim: https://github.com/openzim/libzim (src/fileheader.cpp, fileimpl.cpp, dirent.cpp,
  cluster.cpp, search.cpp, suggestion.cpp, search_iterator.cpp, tools.cpp,
  writer/xapianIndexer.cpp, writer/xapianWorker.cpp, xapian/myhtmlparse.cc)
- libkiwix server: https://github.com/kiwix/libkiwix (src/server/internalServer.cpp,
  search_renderer.cpp, static/templates/search_result.xml)
- kiwix-serve docs: https://kiwix-tools.readthedocs.io/en/latest/kiwix-serve.html
- Kiwix catalog (OPDS): https://library.kiwix.org/catalog/v2/entries

Xapian
- Glass backend sources (1.4): https://github.com/xapian/xapian/tree/RELEASE/1.4/xapian-core/backends/glass
- pack.h (varints): https://github.com/xapian/xapian/blob/RELEASE/1.4/xapian-core/common/pack.h
- BM25: https://github.com/xapian/xapian/blob/RELEASE/1.4/xapian-core/weight/bm25weight.cc
- TermGenerator / QueryParser: https://xapian.org/docs/termgenerator.html ,
  https://xapian.org/docs/apidoc/html/classXapian_1_1QueryParser.html
- Single-file DB (Database(int fd)): https://xapian.org/docs/apidoc/html/classXapian_1_1Database.html ,
  https://trac.xapian.org/ticket/666

Rust crates
- zim 0.5: https://crates.io/crates/zim — tantivy 0.26: https://crates.io/crates/tantivy —
  rmcp 3.4: https://crates.io/crates/rmcp — zstd / ruzstd / liblzma / lzma-rust2 —
  lol_html, scraper, htmd, fast_html2md, dom_smoothie — rust-stemmers, unicode-normalization —
  memmap2, flate2, rayon, notify, fastembed, usearch

Go equivalents (if switching)
- gozim (justinstimatze, cookiengineer), bleve v2.6, klauspost/compress, ulikunitz/xz,
  modelcontextprotocol/go-sdk v1.8, html-to-markdown/v2, readeck/go-readability/v2

Prior art (ZIM + MCP)
- openzim-mcp (Python/libzim, best tool surface): https://github.com/cameronrye/openzim-mcp
- kiwix-mcp (Python/libzim): https://github.com/roanpy/kiwix-mcp
- offline-knowledge (Rust/tantivy/rmcp): https://github.com/kohlhofer/offline-knowledge
- gozim MCP (Go/bleve): https://github.com/justinstimatze/gozim
- zim-mcp (Go/cgo libzim): https://github.com/akhenakh/zim-mcp

Known-issue threads worth reading before P2/P5
- libzim #734 (per-language), #766/#458 (ranking), #952 (boilerplate), #794 (wildcards),
  #731 (spelling), #932 (multi-ZIM suggest), #592 (elision), #1121 (engine survey);
  libkiwix #1159 (multi-language refusal), #395 (async snippets)
