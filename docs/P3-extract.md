# P3 — `zimz-extract`: content to Markdown, text, outlines, links, snippets (2026-09-24)

Scope: PLAN.md §6.4, §7 P3. Turns a ZIM entry into what an agent should read: Markdown
with a heading outline and resolved links, plain text for snippets and word counts.

## What exists (crate `zimz-extract`, ~2 000 lines incl. tests)

| Module | Role |
|---|---|
| `render` | DOM (html5ever via `scraper`) → Markdown or plain text in one walk: headings, paragraphs, nested lists, pipe tables (infoboxes become `- **label:** value` lists), fenced code with language, block quotes, definition lists, `![alt]` images, `$…$` for MathML `alttext`, links rewritten to `zim://<archive>/<path>`. Block content inside inline wrappers (`<a><h2>`, `<summary><h2>`, `<b><p>`) keeps its structure. Sections are located in the final output with byte ranges up to the next heading of the same or higher level. |
| `adapter` | Per-scraper content roots and prune lists: mwoffliner (navboxes, reflists, `sup.reference` and the older `sup.mw-ref`, edit links, hatnotes, empty References/External links headings, footer), devdocs (`._page`, navbar, attribution), zimit/generic (`main`/`article`/`#content` heuristics; nav, header, footer, aside, menus, cookie banners, forms), Gutenberg (PG header/footer, ZIM chrome, page numbers), LibreTexts (stub `index/page_<id>` → `content/page_content_<id>.json` `htmlBody`), YouTube (stub → `videos/<slug>.json` title, channel, date, duration, description, chapters, `video.<lang>.vtt` transcript). Detection from the `Scraper` metadata with `Name` as fallback. |
| `links` | Percent-decoding, `?query`/`#fragment` stripping, `.`/`..` resolution against the entry, old-scheme namespace climbing (`../-/style.css` → `-/style.css`), scheme detection (`mailto:`, `//host`), `javascript:`/`data:` dropped. |
| `document` | `Document { title, markdown, text, sections, links, word_count, adapter, source_path }` with `outline()`, `section_markdown(i)`, `find_section("3" \| "causes")`, `internal_links()`. |
| `snippet` | Best window of `max_chars` covering the most distinct query terms (caller supplies the word→term matcher so stemming stays in `zimz-glass`), grown with context, optional highlighting, `…` markers. |
| `vtt` | WebVTT → transcript (timestamps, cue settings, tags, duplicate cues removed). |

CLI: `zimz extract <zim> <path> [--text] [--outline] [--section <n|title>] [--link-prefix zim://name/]`.

## Verification

- 22 tests: link resolution (relative, `..` clamping, old-scheme namespaces, encoded
  colons, externals, anchors, junk), renderer behaviour on snippets shaped like each
  scraper's markup (sections and ranges, inline markup, nested/ordered lists, tables and
  infoboxes, code fences, quotes, definition lists, hidden/script/style skipping, math,
  block content in inline wrappers, old reference markers), snippets, VTT, adapter
  detection; the mwoffliner fixture (every one of its 3 821 articles extracts; the
  `Climate_change` article's first 30 internal links all resolve to existing entries;
  no `[edit]` or reference lines survive).
- Local library samples (`ZIMZ_TEST_ZIM_DIR`), kept share of the raw page text after
  pruning: Wikipedia `Zstd` 38 % (965 words, 5 sections, 94 links), `Leonardo da Vinci`
  63 % (11 352 words, 30 sections), WikEM (ZIM 5.0) 78 %, devdocs `git-rebase` 100 %
  (8 562 words, 26 sections), ManKier `cat` (zimit) 326 words with 5 sections, Gutenberg
  book 13 664 words / 27 sections, LibreTexts page from its JSON record, ProofWiki 98 %,
  YouTube video from its JSON record with chapters. Extracting a 113 KB Wikipedia page
  takes ~2 ms.

## Known limits

- Table rendering ignores `rowspan`/`colspan`; complex layout tables degrade to one
  line per row.
- Zimit pages depend on the site's own markup; the generic heuristics keep the main
  container and drop nav/header/footer/aside, nothing smarter (a Readability port is
  the planned upgrade if needed).
- YouTube transcripts are only found when `subtitleList` names a language and the
  scraper stored `videos/<id>/video.<lang>.vtt`.
- PDFs, EPUBs and images are not extracted (`Error::Unsupported`).
