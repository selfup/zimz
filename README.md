# zimz

Native Rust tools for [ZIM](https://wiki.openzim.org/wiki/ZIM_file_format) archives
(Wikipedia, DevDocs, Gutenberg, LibreTexts, … as packaged by Kiwix), without libzim or
Xapian: a reader, a search engine over the indexes embedded in every archive, a
Markdown extractor, and an [MCP](https://modelcontextprotocol.io) server that lets AI
agents search a whole directory of archives and read from them.

Status: phases P0–P4 of [PLAN.md](PLAN.md) are implemented; notes with measurements are
in [docs/](docs/). GPL-3.0-only, © Regis Boudinot.

## Crates

| Crate | What it does |
|---|---|
| `zimz-core` | ZIM reader: header, MIME list, directory entries, clusters (none / zstd / xz), title listings, metadata, checksum, split and embedded archives. |
| `zimz-glass` | Reads the Xapian "glass" databases embedded in ZIMs and runs libzim-identical full-text search (BM25) and title suggestions. |
| `zimz-extract` | HTML/JSON entries → Markdown or plain text with an outline, resolved links, per-scraper boilerplate pruning, snippets. |
| `zimz-search` | A directory of archives as one library: catalogue, federated search with rank fusion, articles, links, budgeted context excerpts, health. |
| `zimz-mcp` | MCP server (stdio) over a library: `list_archives`, `search`, `read_article`, `outline`, `suggest`, `context`, `links`, `archive_health`; `zim://` resources. |
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

A typical agent turn: `context` with the question (excerpts from the best sections across
all archives, each cited as `zim://archive/path#Section`), then `read_article` on a
citation for the full text, `max_chars` at a time.

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
scripts/fetch-fixtures.sh            # openzim/zim-testing-suite samples (gitignored)
cargo test --workspace               # unit, fixture, property and MCP end-to-end tests
ZIMZ_TEST_ZIM_DIR=~/zims cargo test --workspace   # also the local-library tests
uv run scripts/search_parity.py …    # python-libzim oracles (see docs/P2-glass-reader.md)
```

`cargo clippy --workspace --all-targets -- -D warnings` must stay clean.
