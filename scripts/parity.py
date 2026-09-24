#!/usr/bin/env python3
"""Produce a parity manifest for one ZIM using python-libzim (the reference reader).

    pip install libzim
    python3 scripts/parity.py ~/zims/foo.zim --count 1000 --seed 1 -o /tmp/foo.json
    ZIMZ_PARITY_MANIFEST=/tmp/foo.json cargo test -p zimz-core --test parity -- --nocapture
"""
import argparse, hashlib, json, random, sys
from libzim.reader import Archive

ap = argparse.ArgumentParser()
ap.add_argument("zim")
ap.add_argument("--count", type=int, default=1000)
ap.add_argument("--seed", type=int, default=1)
ap.add_argument("--title-lookups", type=int, default=50)
ap.add_argument("-o", "--output", required=True)
args = ap.parse_args()

a = Archive(args.zim)
rng = random.Random(args.seed)
n = a.all_entry_count
indexes = sorted(set([0, n - 1] + [rng.randrange(n) for _ in range(args.count)]))

entries = []
for i in indexes:
    e = a._get_entry_by_id(i)
    rec = {"index": i, "path": e.path, "title": e.title, "is_redirect": e.is_redirect,
           "mimetype": None, "size": None, "md5": None, "redirect_path": None}
    if e.is_redirect:
        rec["redirect_path"] = e.get_item().path
    else:
        item = e.get_item()
        rec["mimetype"] = item.mimetype
        rec["size"] = item.size
        # huge blobs (embedded Xapian indexes can be gigabytes) are compared by size only
        if item.size <= 64 * 1024 * 1024:
            rec["md5"] = hashlib.md5(bytes(item.content)).hexdigest()
    entries.append(rec)

metadata = []
for key in a.metadata_keys:
    if key.startswith("Illustration"):
        continue
    try:
        metadata.append((key, a.get_metadata(key).decode("utf-8")))
    except Exception:
        pass

title_lookups = []
if a.has_title_index or True:
    for _ in range(args.title_lookups):
        e = a._get_entry_by_id(rng.randrange(n))
        try:
            t = a.get_entry_by_title(e.title)
            title_lookups.append({"title": e.title, "path": t.path})
        except Exception:
            pass

main_path = None
try:
    main_path = a.main_entry.get_item().path
except Exception:
    pass

json.dump({"zim": args.zim, "uuid": str(a.uuid), "all_entry_count": n, "main_path": main_path,
           "metadata": metadata, "entries": entries, "title_lookups": title_lookups},
          open(args.output, "w"))
print(f"{args.zim}: {len(entries)} entries, {len(title_lookups)} title lookups -> {args.output}")
