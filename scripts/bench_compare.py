#!/usr/bin/env python3
"""Compare python-libzim (the reference C++ reader) with zimz-core on one workload.

    uv run scripts/bench_compare.py ~/zims/wikispecies_en_all_maxi_2026-04.zim [more.zim ...]
        [--index-lookups 5000] [--path-lookups 2000] [--title-lookups 500] [--cold-items 300]
        [--scan-entries 50000] [--scan-max-mb 256] [--checksum] [--seed 1] [--json-out FILE]

For each archive a seeded workload (entry indexes, paths, titles, items spread across the
file) is generated with python-libzim and written to target/bench/<name>.workload.json.
Both implementations then run the same operations on it: python-libzim in this process,
zimz-core via `cargo run --release -p zimz-core --example bench_compare`, which prints
its timings as JSON. Times are measured inside each implementation's own loop, so cargo
compile time and process start-up are excluded.

Caveats: the Python numbers include interpreter and binding overhead (roughly 0.3-1 µs
per call), which matters for the cheap operations (entry by index, warm reads) and is
negligible for decompression-bound ones (cold reads, scan). Run python-libzim first so
both sides see a warm OS page cache.
"""
import argparse, json, os, random, subprocess, sys, time
from pathlib import Path

from libzim.reader import Archive

ROOT = Path(__file__).resolve().parent.parent


def make_workload(zim: str, args) -> dict:
    a = Archive(zim)
    n = a.all_entry_count
    rng = random.Random(args.seed)
    index_lookups = [rng.randrange(n) for _ in range(args.index_lookups)]
    path_lookups = []
    for i in index_lookups:
        # only user-content entries resolve through get_entry_by_path (M/W/X do not)
        p = a._get_entry_by_id(i).path
        if a.has_entry_by_path(p):
            path_lookups.append(p)
        if len(path_lookups) >= args.path_lookups:
            break
    title_lookups = []
    for i in rng.sample(range(n), min(n, args.title_lookups * 4)):
        e = a._get_entry_by_id(i)
        try:
            if a.get_entry_by_title(e.title)._index == i:
                title_lookups.append(e.title)
        except Exception:
            pass
        if len(title_lookups) >= args.title_lookups:
            break
    cold_items = []
    stride = max(1, n // (args.cold_items * 2))
    for k in range(0, n, stride):
        i = min(n - 1, k + rng.randrange(stride))
        e = a._get_entry_by_id(i)
        if not e.is_redirect and e.get_item().size <= 2 << 20:
            cold_items.append(i)
        if len(cold_items) >= args.cold_items:
            break
    return {
        "zim": zim,
        "seed": args.seed,
        "index_lookups": index_lookups,
        "path_lookups": path_lookups,
        "title_lookups": title_lookups,
        "cold_items": cold_items,
        "scan_entries": min(n, args.scan_entries),
        "scan_max_bytes": args.scan_max_mb << 20,
        "checksum": bool(args.checksum),
    }


def touch(mv) -> int:
    """Read one byte per 4 KiB page so the blob is really materialised (mirrors the Rust side)."""
    if mv.format != "B":
        mv = mv.cast("B")
    return sum(mv[::4096]) + len(mv)


def bench_python(w: dict) -> dict:
    zim = w["zim"]
    r = {}
    t = []
    for _ in range(5):
        t0 = time.perf_counter_ns()
        Archive(zim)
        t.append(time.perf_counter_ns() - t0)
    r["open_us"] = min(t) / 1e3
    a = Archive(zim)
    acc = 0
    sink = 0

    # Every random-access loop runs twice: the first pass is reported as "*_first"
    # (page cache in whatever state it was), the second as the headline number (warm).
    def timed(loop, key, n):
        nonlocal acc
        for suffix in ("_first_ns", "_ns"):
            t0 = time.perf_counter_ns()
            loop()
            r[key + suffix] = (time.perf_counter_ns() - t0) / max(1, n)

    idx = w["index_lookups"]

    def by_index():
        nonlocal acc
        for i in idx:
            acc += len(a._get_entry_by_id(i).path)

    timed(by_index, "entry_by_index", len(idx))

    paths = w["path_lookups"]

    def by_path():
        nonlocal acc
        for p in paths:
            acc += a.get_entry_by_path(p)._index

    timed(by_path, "path_lookup", len(paths))

    titles = w["title_lookups"]
    if titles:

        def by_title():
            nonlocal acc
            for s in titles:
                acc += a.get_entry_by_title(s)._index

        timed(by_title, "title_lookup", len(titles))

    cold = w["cold_items"]
    for suffix in ("_first", ""):
        a = Archive(zim)  # fresh reader caches; the second pass has a warm page cache
        t0 = time.perf_counter_ns()
        nbytes = 0
        for i in cold:
            mv = a._get_entry_by_id(i).get_item().content
            nbytes += len(mv)
            sink += touch(mv)
        dt = time.perf_counter_ns() - t0
        r["cold_read" + suffix + "_us"] = dt / 1e3 / max(1, len(cold))
        r["cold_read" + suffix + "_mb_s"] = nbytes / 1e6 / (dt / 1e9) if dt else 0.0

    warm = cold[:10]
    t0 = time.perf_counter_ns()
    for _ in range(20):
        for i in warm:
            sink += touch(a._get_entry_by_id(i).get_item().content)
    r["warm_read_ns"] = (time.perf_counter_ns() - t0) / max(1, 20 * len(warm))

    for suffix in ("_first", ""):
        a = Archive(zim)
        t0 = time.perf_counter_ns()
        items = 0
        nbytes = 0
        for i in range(w["scan_entries"]):
            e = a._get_entry_by_id(i)
            if not e.is_redirect:
                mv = e.get_item().content
                nbytes += len(mv)
                sink += touch(mv)
                items += 1
                if nbytes >= w["scan_max_bytes"]:
                    break
        dt = time.perf_counter_ns() - t0
        r["scan" + suffix + "_items"] = items
        r["scan" + suffix + "_mb"] = nbytes / 1e6
        r["scan" + suffix + "_ms"] = dt / 1e6
        r["scan" + suffix + "_mb_s"] = nbytes / 1e6 / (dt / 1e9) if dt else 0.0
        r["scan" + suffix + "_items_s"] = items / (dt / 1e9) if dt else 0.0

    if w["checksum"]:
        t0 = time.perf_counter_ns()
        ok = a.check()
        r["checksum_ms"] = (time.perf_counter_ns() - t0) / 1e6
        r["checksum_ok"] = bool(ok)
    r["_acc"] = acc + sink
    return r


def bench_rust(workload_path: Path) -> dict:
    cmd = ["cargo", "run", "-q", "--release", "-p", "zimz-core", "--example", "bench_compare", "--", str(workload_path)]
    out = subprocess.run(cmd, cwd=ROOT, check=True, capture_output=True, text=True).stdout
    return json.loads(out)


ROWS = [
    ("open (min of 5)", "open_us", "µs", "lower"),
    ("entry by index, first pass", "entry_by_index_first_ns", "ns", "lower"),
    ("entry by index, warm", "entry_by_index_ns", "ns", "lower"),
    ("path lookup, first pass", "path_lookup_first_ns", "ns", "lower"),
    ("path lookup, warm", "path_lookup_ns", "ns", "lower"),
    ("title lookup, first pass", "title_lookup_first_ns", "ns", "lower"),
    ("title lookup, warm", "title_lookup_ns", "ns", "lower"),
    ("item read, fresh reader caches, first pass", "cold_read_first_us", "µs", "lower"),
    ("item read, fresh reader caches, warm pages", "cold_read_us", "µs", "lower"),
    ("… as throughput", "cold_read_mb_s", "MB/s", "higher"),
    ("item read, cached cluster", "warm_read_ns", "ns", "lower"),
    ("scan throughput, first pass", "scan_first_mb_s", "MB/s", "higher"),
    ("scan throughput, warm pages", "scan_mb_s", "MB/s", "higher"),
    ("scan items/s, warm pages", "scan_items_s", "/s", "higher"),
    ("checksum (MD5 of file)", "checksum_ms", "ms", "lower"),
]


def fmt(v, unit):
    if v is None:
        return "-"
    if unit in ("ns", "µs", "ms") and v >= 1000 and unit != "ms":
        return f"{v / 1000:,.1f} {'µs' if unit == 'ns' else 'ms'}"
    return f"{v:,.1f} {unit}"


def table(py: dict, rs: dict, w: dict) -> str:
    lines = [
        f"### {os.path.basename(w['zim'])}  ({w['scan_entries']} scanned entries, {len(w['index_lookups'])} index lookups, {len(w['cold_items'])} cold items)",
        "",
        "| operation | python-libzim | zimz-core | zimz vs python |",
        "|---|---|---|---|",
    ]
    for label, key, unit, better in ROWS:
        p, r = py.get(key), rs.get(key)
        if p is None and r is None:
            continue
        if p and r:
            ratio = (p / r) if better == "lower" else (r / p)
            rel = f"{ratio:.1f}x faster" if ratio >= 1 else f"{1 / ratio:.1f}x slower"
        else:
            rel = "-"
        lines.append(f"| {label} | {fmt(p, unit)} | {fmt(r, unit)} | {rel} |")
    lines.append("")
    lines.append(f"scan: python {py['scan_items']} items / {py['scan_mb']:.1f} MB in {py['scan_ms']:.0f} ms; zimz {rs['scan_items']} items / {rs['scan_mb']:.1f} MB in {rs['scan_ms']:.0f} ms")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("zims", nargs="+")
    ap.add_argument("--index-lookups", type=int, default=5000)
    ap.add_argument("--path-lookups", type=int, default=2000)
    ap.add_argument("--title-lookups", type=int, default=500)
    ap.add_argument("--cold-items", type=int, default=300)
    ap.add_argument("--scan-entries", type=int, default=50_000)
    ap.add_argument("--scan-max-mb", type=int, default=256)
    ap.add_argument("--checksum", action="store_true", help="also time full-file MD5 verification")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--regen", action="store_true", help="regenerate cached workloads")
    ap.add_argument("--json-out", type=Path)
    args = ap.parse_args()

    out_dir = ROOT / "target" / "bench"
    out_dir.mkdir(parents=True, exist_ok=True)
    all_results = []
    for zim in args.zims:
        zim = os.path.abspath(zim)
        name = os.path.basename(zim)
        wl_path = out_dir / f"{name}.workload.json"
        if args.regen or not wl_path.exists():
            print(f"generating workload for {name} …", file=sys.stderr)
            wl_path.write_text(json.dumps(make_workload(zim, args)))
        w = json.loads(wl_path.read_text())
        w["checksum"] = bool(args.checksum)
        wl_path.write_text(json.dumps(w))
        print(f"python-libzim on {name} …", file=sys.stderr)
        py = bench_python(w)
        print(f"zimz-core on {name} …", file=sys.stderr)
        rs = bench_rust(wl_path)
        print(table(py, rs, w))
        print()
        all_results.append({"zim": zim, "python_libzim": py, "zimz_core": rs, "workload": {k: v for k, v in w.items() if not isinstance(v, list)}})
    if args.json_out:
        args.json_out.write_text(json.dumps(all_results, indent=1))


if __name__ == "__main__":
    main()
