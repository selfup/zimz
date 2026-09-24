#!/usr/bin/env python3
"""Record python-libzim's search results for a set of queries (ranking oracle).

    uv run scripts/search_parity.py <file.zim> -o target/parity/search-foo.json [-- "query one" "query two" …]
    ZIMZ_SEARCH_PARITY=target/parity/search-foo.json cargo test -p zimz-glass --test search_parity -- --nocapture

Without explicit queries a default list is chosen from the archive name. The manifest
holds, per query, the top-N result paths in libzim's order and its estimated match count;
`tests/search_parity.rs` runs the same queries through zimz-glass and compares.
"""
import argparse, json, os, sys

from libzim.reader import Archive
from libzim.search import Query, Searcher

DEFAULTS = {
    "climate": ["climate change", "sea level rise", "carbon dioxide", "greenhouse gas emissions", "ice sheet", "temperature record",
                "renewable energy", "ocean acidification", "methane", "paris agreement", "coral reef", "drought", "solar", "wind power",
                "deforestation", "arctic", "glacier retreat", "extreme weather", "fossil fuels", "biodiversity"],
    "devdocs": ["commit", "rebase branch", "merge conflict", "git log", "stash", "submodule update", "cherry pick", "reflog", "bisect",
                "diff", "remote origin", "tag", "worktree", "blame", "fetch"],
    "wikem": ["chest pain", "sepsis", "head injury", "myocardial infarction", "asthma", "pneumonia", "stroke", "fracture", "seizure",
              "hypertension", "kidney stone", "appendicitis", "burn", "dehydration", "anaphylaxis"],
    "generic": ["history", "water", "energy", "war", "science", "music", "river", "mountain", "king", "city", "language",
                "animal", "plant", "disease", "computer"],
}


def pick_queries(name: str) -> list[str]:
    for key, qs in DEFAULTS.items():
        if key in name:
            return qs
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
    if not a.has_fulltext_index:
        print("archive has no fulltext index", file=sys.stderr)
        return 1
    searcher = Searcher(a)
    queries = args.queries or pick_queries(os.path.basename(zim))
    out = []
    for q in queries:
        search = searcher.search(Query().set_query(q))
        est = search.getEstimatedMatches()
        paths = list(search.getResults(0, args.top))
        out.append({"query": q, "estimated": est, "paths": paths})
    json.dump({"zim": zim, "new_scheme": a.has_new_namespace_scheme, "top": args.top, "language": a.get_metadata("Language").decode(),
               "queries": out}, open(args.output, "w"), indent=1)
    print(f"{zim}: {len(out)} queries -> {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
