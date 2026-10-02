#!/usr/bin/env bash
# Run the zimz benchmark harnesses against local ZIM archives and write a dated results
# document. Three harnesses are exercised:
#   crates/zimz-core/examples/bench.rs      reader micro-timings (open, lookup, decode)
#   crates/zimz-search/examples/profile.rs  federated search + context over a small set
#   scripts/bench_compare.py                head-to-head vs python-libzim (needs uv)
#
# Usage: scripts/bench.sh [ZIM_DIR] [--out FILE] [--query "…"]
#   ZIM_DIR  directory of *.zim archives (default: $ZIMZ_BENCH_ZIM_DIR, else ~/zims)
#   --out    markdown output (default: docs/bench-<date>.md)
#   --query  query for the federated-search step (default: "borrow checker lifetimes")
#
# Env:
#   ZIMZ_BENCH_MAX_MB  size cap (MiB) for the archive used in the libzim comparison;
#                      the largest archive at or below it is chosen (default 4096)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

QUERY="borrow checker lifetimes"
MAX_MB="${ZIMZ_BENCH_MAX_MB:-4096}"
OUT=""
ZIM_DIR=""

while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --query) QUERY="$2"; shift 2 ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) ZIM_DIR="$1"; shift ;;
  esac
done

ZIM_DIR="${ZIM_DIR:-${ZIMZ_BENCH_ZIM_DIR:-$HOME/zims}}"
[ -d "$ZIM_DIR" ] || { echo "no such directory: $ZIM_DIR" >&2; exit 1; }

DATE="$(date +%F)"
OUT="${OUT:-docs/bench-$DATE.md}"

# File size without reading the file (some archives here are > 100 GiB).
size() { stat -f%z "$1" 2>/dev/null || stat -c%s "$1"; }

# Smallest devdocs archive and the wiki used as a mid-size reader target.
small_devdocs="$(ls -Sr "$ZIM_DIR"/devdocs_en_*.zim 2>/dev/null | head -1)"
wikem="$(ls "$ZIM_DIR"/wikem_en_*.zim 2>/dev/null | head -1)"

# Largest archive at or below MAX_MB, for the libzim comparison (ascending -> last wins).
compare=""
while IFS= read -r f; do
  [ -n "$f" ] || continue
  [ "$(size "$f")" -le $(( MAX_MB * 1048576 )) ] && compare="$f"
done < <(ls -Sr "$ZIM_DIR"/*.zim 2>/dev/null)

[ -n "$small_devdocs$wikem$compare" ] || { echo "no *.zim under $ZIM_DIR" >&2; exit 1; }

# A handful of self-contained archives, symlinked into a scratch dir, so the federated
# search step does not fan out over the whole library (hundreds of files, some huge).
# Prefer the larger devdocs (content-rich enough to return hits) plus the wiki, and cap
# the size so mmap-ing them is cheap.
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
n=0
while IFS= read -r f; do
  [ -n "$f" ] || continue
  [ "$(size "$f")" -le $(( 64 * 1048576 )) ] || continue
  ln -sf "$(cd "$(dirname "$f")" && pwd)/$(basename "$f")" "$tmp/$(basename "$f")"
  n=$((n + 1))
  [ "$n" -ge 4 ] && break
done < <(ls -S "$ZIM_DIR"/devdocs_en_*.zim 2>/dev/null)
if [ -n "$wikem" ]; then
  ln -sf "$(cd "$(dirname "$wikem")" && pwd)/$(basename "$wikem")" "$tmp/$(basename "$wikem")"
fi

mkdir -p target/bench
host="$(uname -srm)"
rustc_version="$(rustc --version)"
git_sha="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"

echo "writing $OUT" >&2

{
  echo "# zimz benchmarks ($DATE)"
  echo
  echo "- host: $host"
  echo "- $rustc_version"
  echo "- git $git_sha"
  echo "- archives under: \`$ZIM_DIR\`"
  echo
  echo "Regenerate with \`scripts/bench.sh $ZIM_DIR\`. Latency targets: \`docs/P1-core-reader.md\` (§Timings) and \`docs/P2-glass-reader.md\`. The head-to-head method and interpretation are in \`docs/bench-vs-python-libzim.md\`."
  echo
  echo "## Reader micro-timings (\`crates/zimz-core/examples/bench.rs\`, 3000 samples)"
  echo

  for zim in "$small_devdocs" "$wikem" "$compare"; do
    [ -n "$zim" ] || continue
    echo "### $(basename "$zim")"
    echo
    echo '| operation | result |'
    echo '|---|---|'
    cargo run --quiet --release -p zimz-core --example bench -- "$zim" 3000 2>/dev/null \
      | awk -F': ' '/: / { printf "| %s | %s |\n", $1, $2 }'
    echo
  done

  echo "## Federated search (\`crates/zimz-search/examples/profile.rs\`)"
  echo
  echo "Query: \`$QUERY\` over a few small devdocs archives plus the wiki."
  echo
  echo '```'
  cargo run --quiet --release -p zimz-search --example profile -- "$tmp" "$QUERY" 2>/dev/null
  echo '```'
  echo

  echo "## Reader vs python-libzim (\`scripts/bench_compare.py\`)"
  echo
  if [ -n "$compare" ]; then
    name="$(basename "$compare" .zim)"
    uv run scripts/bench_compare.py "$compare" \
      --index-lookups 3000 --path-lookups 1000 --title-lookups 300 \
      --cold-items 200 --scan-entries 20000 \
      --json-out "target/bench/$name.json" 2>&1 \
      | grep -vE '^(generating workload|python-libzim on|zimz-core on)'
  else
    echo "no archive at or below ZIMZ_BENCH_MAX_MB=${MAX_MB} MiB; skipped"
  fi
} > "$OUT"

echo "done: $OUT" >&2
