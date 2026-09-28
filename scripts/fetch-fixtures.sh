#!/usr/bin/env bash
# Download the openZIM testing suite (small ZIM files, valid and deliberately broken)
# into fixtures/zim-testing-suite/. Pinned to a commit for reproducibility; override with
# ZIM_TESTING_SUITE_REF=<sha or branch>.
#
# Uses the codeload tarball, not the REST API: unauthenticated API calls are limited to
# 60 an hour per IP, which shared CI runners exhaust (HTTP 403).
set -euo pipefail

REPO="openzim/zim-testing-suite"
REF="${ZIM_TESTING_SUITE_REF:-2edf72096208e60c82b6ebbe313baa552cc6af52}"
DEST="$(cd "$(dirname "$0")/.." && pwd)/fixtures/zim-testing-suite"

if [ -f "$DEST/VERSION" ] && [ "$(cat "$DEST/VERSION")" = "$REF" ]; then
  echo "fixtures already at $REF"; exit 0
fi

echo "fetching $REPO @ $REF"

tmp="$(mktemp -d)"

trap 'rm -rf "$tmp"' EXIT

curl -sSfL --retry 3 "https://codeload.github.com/$REPO/tar.gz/$REF" | tar -xzf - -C "$tmp"

rm -rf "$DEST/data"

mkdir -p "$DEST"

mv "$tmp"/*/data "$DEST/data"

echo "$REF" > "$DEST/VERSION"

echo "done: $(find "$DEST/data" -type f | wc -l | tr -d ' ') files"
