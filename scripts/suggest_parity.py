#!/usr/bin/env python3
"""Record python-libzim's title suggestions for a set of prefixes (ranking oracle).

    uv run scripts/suggest_parity.py <file.zim> -o target/parity/suggest-foo.json [-- "climate ch" "sea l" …]
    ZIMZ_SUGGEST_PARITY=target/parity/suggest-foo.json cargo test -p zimz-glass --test suggest_parity -- --nocapture
"""
import argparse, json, os, sys

from libzim.reader import Archive
from libzim.suggestion import SuggestionSearcher

DEFAULTS = {
    "climate": ["climate ch", "sea l", "carbon", "glacier", "ice", "green", "temperature", "el ni", "paris", "coral", "climate change in", "effects of", "list of", "global w", "a", "solar"],
    "devdocs": ["git re", "git", "com", "merge", "rebase", "api", "sub", "git log", "stash", "wor"],
    "wikem": ["chest", "head in", "sep", "myo", "acute", "pediatric", "burn", "fracture", "cardiac", "hyp", "a", "pneumo"],
    "generic": ["the", "a", "list of", "history of", "john", "new", "war", "river", "united", "battle of", "king", "music", "lake", "san"],
}


def pick(name: str) -> list[str]:
    for k, v in DEFAULTS.items():
        if k in name:
            return v
    return DEFAULTS["generic"]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("zim")
    ap.add_argument("-o", "--output", required=True)
    ap.add_argument("-n", "--top", type=int, default=10)
    ap.add_argument("queries", nargs="*")
    args = ap.parse_args()
    zim = os.path.abspath(args.zim)
    a = Archive(zim)
    if not a.has_title_index:
        print("archive has no title index", file=sys.stderr)
        return 1
    ss = SuggestionSearcher(a)
    out = []
    for q in args.queries or pick(os.path.basename(zim)):
        r = ss.suggest(q)
        out.append({"query": q, "estimated": r.getEstimatedMatches(), "paths": list(r.getResults(0, args.top))})
    json.dump({"zim": zim, "new_scheme": a.has_new_namespace_scheme, "top": args.top, "queries": out}, open(args.output, "w"), indent=1)
    print(f"{zim}: {len(out)} suggestion queries -> {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
