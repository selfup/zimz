# zimz

<p align="center">
  <img src="https://gitlab.com/-/project/86917340/uploads/4d10d1691a03afaa9e33cc22dea7e26c/zimz_banner.png" alt="zimz banner" width="300" height="300">
</p>

Native Rust tools for [ZIM](https://wiki.openzim.org/wiki/ZIM_file_format) archives
(Wikipedia, DevDocs, Gutenberg, LibreTexts, … as packaged by Kiwix), without libzim or
Xapian: a reader, a search engine over the indexes embedded in every archive, a
Markdown extractor, and an [MCP](https://modelcontextprotocol.io) server that lets AI
agents search a whole directory of archives and read from them.

Status: phases P0–P4 of [PLAN.md](PLAN.md) are implemented; notes with measurements are
in [docs/](docs/). GPL-3.0-or-later, © Regis Boudinot. The Xapian glass reader and the ZIM
format handling are derived from xapian-core and libzim (GPL-2.0-or-later); see
[COPYRIGHT](COPYRIGHT) for the preserved notices.

## Crates

| Crate | What it does |
|---|---|
| `zimz-core` | ZIM reader: header, MIME list, directory entries, clusters (none / zstd / xz), title listings, metadata, checksum, split and embedded archives. |
| `zimz-glass` | Reads the Xapian "glass" databases embedded in ZIMs and runs libzim-identical full-text search (BM25) and title suggestions. |
| `zimz-extract` | HTML/JSON entries → Markdown or plain text with an outline, resolved links, per-scraper boilerplate pruning, snippets. |
| `zimz-search` | A directory of archives as one library: catalogue, federated search with rank fusion, articles, links, budgeted context excerpts, health. |
| `zimz-mcp` | MCP server (stdio, or streamable HTTP with a bearer token) over a library: `list_archives`, `search`, `read_article`, `outline`, `suggest`, `context`, `links`, `archive_health`; `zim://` resources. |
| `zimz-cli` | The `zimz` binary. |

## Use with Claude Code (or any MCP client)

```sh
cargo install --path crates/zimz-cli          # installs `zimz`
claude mcp add zimz -- zimz mcp --zim-dir ~/zims
```

or in `.mcp.json`:

```json
{
  "mcpServers": {
    "zimz": { "command": "zimz", "args": ["mcp", "--zim-dir", "/Users/you/zims"] }
  }
}
```

Options: `--zim-dir` (repeatable, recursive; `--no-recursive` to stay flat), `--zim FILE`,
`--cluster-cache-mb` (default 256, shared by all archives), `--extract-cache-mb` (64),
`--priority 'wikipedia_*=2'` (ranking weight per archive glob). Logs go to stderr;
`ZIMZ_LOG=debug` for more.

### One shared server over HTTP (local or LAN)

```sh
zimz mcp --zim-dir ~/zims --http                       # 127.0.0.1:8765, this machine only
zimz mcp --zim-dir ~/zims --http 0.0.0.0:8765 --token "$(openssl rand -hex 24)"   # LAN
claude mcp add --transport http zimz http://127.0.0.1:8765/mcp --header "Authorization: Bearer <token>"
```

One warm process (one mmap of the library, shared caches) serves every client. A
non-loopback bind refuses to start without `--token` (or `ZIMZ_TOKEN`); with a token,
requests must carry `Authorization: Bearer <token>`. `GET /healthz` is open. Put TLS in
front with a reverse proxy if the network is not trusted. `archive_health` with
`verify: full` is disabled on HTTP because it reads whole archives.

Queries can quote phrases: `"borrow checker" lifetimes` requires the two words in that
order somewhere in the article (verified in the extracted text, on top of the usual
stemmed AND search).

A typical agent turn: `context` with the question (excerpts from the best sections across
all archives, each cited as `zim://archive/path#Section`), then `read_article` on a
citation for the full text, `max_chars` at a time.

## Tools

Every tool is read-only and idempotent, returns structured JSON plus a text rendering,
and reports library errors (unknown archive, missing article, bad cursor) as tool errors
the model can read and correct.

| Tool | Parameters | Behaviour |
|---|---|---|
| `list_archives` | `filter` | Catalogue: name (use it in every other tool), title, description, language, date, size, article / HTML / media counts, index presence, `search_mode` (`fulltext`, `title` or `listing`), priority. `filter` is a substring over the descriptive fields. |
| `search` | `query`, `archives`, `mode`, `limit` (≤ 50), `cursor`, `snippet_chars`, `min_score`, `or_fallback` | Stemmed AND of the words per archive language; `"quoted phrases"` are verified in the article text. Rankings are fused across archives (reciprocal rank fusion with exact-title and match-strength boosts, weighted by `--priority`), scores normalised to 1.0 for the best hit. When AND fills less than a page, OR matches follow the AND hits flagged `partial` (`or_fallback: false` disables). `archives` takes names, uuids, file stems or globs (`devdocs_*`); `mode` forces `fulltext`, `title` or `listing` (`auto` picks the best available). Pages continue with `next_cursor`; `min_score` drops weak hits. |
| `read_article` | `archive`, `path`, `format`, `max_chars`, `offset`, `section` | Markdown (default), plain `text` or raw `html`, never more than `max_chars` characters; when cut, the response carries the outline and a `next_offset`. `section` selects one heading by index or (prefix of) title. `path` accepts a search-hit path, a `zim://` URI, `C/…` or `A/…`, percent-encoded or spaced forms, or a title; redirects are followed and reported. |
| `outline` | `archive`, `path` | Heading tree with the size of each section. |
| `links` | `archive`, `path`, `limit`, `offset` | Internal links with anchor text, canonical target path, title and whether the target exists. |
| `suggest` | `prefix`, `archives`, `limit` | Title completion across the title indexes (listing prefix scan when an archive has none), exact titles first. |
| `context` | `query`, `archives`, `mode`, `budget_chars`, `per_hit_chars`, `max_hits` | One call: search, pick the best section of each top hit, pack excerpts under the budget, cite each as `zim://archive/path#Section`. Phrases and `archives` work as in `search`. |
| `archive_health` | `archive`, `verify` | Index presence and coverage (full-text docs over HTML items), open time, cache statistics, scan failures; `verify: quick` runs structural checks, `full` adds the checksum and every cluster (slow; refused over HTTP). |

Resources: `zim://{archive}` (catalogue entry as JSON) and `zim://{archive}/{path}` (whole
article as Markdown; `?format=text` or `?format=html`).

## Command line

```sh
zimz archives --zim-dir ~/zims                   # the catalogue
zimz search ~/zims "carbon capture" --archive 'wikipedia_*'
zimz context ~/zims "how does git rebase --onto work"
zimz suggest ~/zims "photosyn"
zimz info  file.zim                              # header, metadata, indexes
zimz ls    file.zim --ns C --prefix Carb
zimz cat   file.zim Carbon_dioxide               # raw entry
zimz extract file.zim Carbon_dioxide --outline   # Markdown / text / sections
zimz search  file.zim "carbon dioxide" -n 5      # one archive's embedded index
zimz check   file.zim --checksum
```

## Development

```sh
scripts/fetch-fixtures.sh            # openzim/zim-testing-suite samples (gitignored; needs uv)
cargo test --workspace               # unit, fixture, property and MCP end-to-end tests
ZIMZ_TEST_ZIM_DIR=~/zims cargo test --workspace   # also the local-library tests
uv run scripts/search_parity.py …    # python-libzim oracles (see docs/P2-glass-reader.md)
```

`cargo clippy --workspace --all-targets -- -D warnings` must stay clean.
