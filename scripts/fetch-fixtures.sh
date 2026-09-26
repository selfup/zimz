#!/usr/bin/env bash
# Download the openZIM testing suite (small ZIM files, valid and deliberately broken)
# into fixtures/zim-testing-suite/. Pinned to a commit for reproducibility.
set -euo pipefail
REPO="openzim/zim-testing-suite"
REF="${ZIM_TESTING_SUITE_REF:-main}"
DEST="$(cd "$(dirname "$0")/.." && pwd)/fixtures/zim-testing-suite"
mkdir -p "$DEST"
# Python runs through uv only (see CLAUDE.md); the scripts need nothing beyond the stdlib.
PY="uv run --quiet --project $(dirname "$DEST") python"
sha=$(curl -sSf "https://api.github.com/repos/$REPO/commits/$REF" | $PY -c 'import json,sys; print(json.load(sys.stdin)["sha"])')
if [ -f "$DEST/VERSION" ] && [ "$(cat "$DEST/VERSION")" = "$sha" ]; then
  echo "fixtures already at $sha"; exit 0
fi
echo "fetching $REPO @ $sha"
curl -sSf "https://api.github.com/repos/$REPO/git/trees/$sha?recursive=1" \
  | $PY -c 'import json,sys; [print(e["path"]) for e in json.load(sys.stdin)["tree"] if e["type"]=="blob" and e["path"].startswith("data/")]' \
  | while read -r path; do
      mkdir -p "$DEST/$(dirname "$path")"
      curl -sSf -o "$DEST/$path" "https://raw.githubusercontent.com/$REPO/$sha/$path"
    done
echo "$sha" > "$DEST/VERSION"
echo "done: $(find "$DEST/data" -type f | wc -l | tr -d ' ') files"
