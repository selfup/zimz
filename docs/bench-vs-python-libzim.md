# zimz-core vs python-libzim (2026-09-24)

`scripts/bench_compare.py` runs one seeded workload through both readers and prints a
table per archive. python-libzim wraps the reference C++ libzim (and the same libzstd we
link), so this measures reader design and binding overhead, not compression codecs.

## Method

- **One workload per archive**, generated once with python-libzim and cached in
  `target/bench/<name>.workload.json`: random entry indexes, user paths, titles that
  resolve, items spread across the file (≤ 2 MiB), a scan budget.
- **Same loops on both sides.** Python times its loops with `perf_counter_ns`; the Rust
  half (`crates/zimz-core/examples/bench_compare.rs`) is launched with
  `cargo run --release` and prints JSON. Compile time and process start-up are excluded.
- **Bytes are materialised on both sides**: every blob is "touched" one byte per 4 KiB
  page, so a zero-copy slice cannot win by never reading the data.
- **Two passes for random access.** OS page-cache state dominates random reads on a 124 GB
  file and varied by 10x between runs. Every random-access loop therefore runs twice; the
  headline number is the second (warm) pass, the first pass is reported separately.
  "Fresh reader caches" means a new `Archive` object, i.e. every cluster is decoded again.
- **Untimed warm-up** of 64 lookups lets lazily built structures settle (zimz builds its
  lookup grids after 8 lookups; libzim builds its grid at open).

Run:

    uv run scripts/bench_compare.py ~/zims/wikispecies_en_all_maxi_2026-04.zim [more.zim …] [--checksum]
    uv run scripts/bench_compare.py ~/zims/wikipedia_en_all_maxi_2026-02.zim \
        --index-lookups 2000 --path-lookups 500 --title-lookups 200 --cold-items 100 --scan-entries 3000 --scan-max-mb 64

## Results (Apple Silicon laptop, NVMe, python-libzim 3.13 / libzim 9.x, zimz-core 0.1)

Ratios are zimz relative to python-libzim; > 1 means zimz is faster.

| operation | Wikipedia 124 GB | wikispecies 3.4 GB | iFixit 3.6 GB | wikem 42 MB (ZIM 5.0) | devdocs 1.5 MB |
|---|---|---|---|---|---|
| open | 6.3x | 40x | 10x | 16x | 4.5x |
| entry by index, warm | 31x (22 ns vs 669 ns) | 2.7x | 2.5x | 36x | 7.8x |
| path lookup, warm | 2.9x (3.4 µs vs 9.6 µs) | 3.1x | 2.9x | 5.5x | 1.0x |
| title lookup, warm | 11x (1.0 µs vs 11 µs) | 3.5x | 8.2x | 5.7x | 1.2x |
| path lookup, first pass | 0.4x (31 µs vs 13 µs) | 1.9x | 2.2x | 3.2x | 1.0x |
| item read, fresh reader caches | 0.8x (344 µs vs 287 µs) | 1.0x | 0.9x | 3.0x | 1.0x |
| item read, cached cluster | 900x (28 ns vs 25 µs) | 1.3x | 1.1x | 335x | 24x |
| scan throughput, warm | 1.5x (43 vs 29 MB/s) | 1.0x | 3.3x (1.36 GB/s vs 0.41) | 41x (5.3 GB/s vs 0.13) | 1.0x |
| checksum | – | 1.2x | 1.2x | 1.1x | 0.7x |

Full tables with absolute numbers: `target/bench/results-*.json` (regenerate with the
commands above; the JSON is not committed).

## Reading the numbers

- **Lookups and open are where the native reader wins** (3–40x warm). The remaining cost
  in both readers is dirent page faults; zimz additionally avoids Python object creation.
- **Cluster decoding is a wash.** Both link libzstd, and a cold item read is one ~2 MiB
  cluster decode. zimz is 10–20 % slower on Wikipedia and iFixit because it decodes the
  whole cluster into memory, while libzim streams and stops at the requested blob. An
  early-stop decode (open item in PLAN.md §2) would close that gap for blobs early in a cluster.
- **First-pass lookups on Wikipedia are 2–2.5x slower** than libzim. libzim samples its
  1024-entry grid at open, so its very first lookups already touch few pages; zimz builds
  its grids (every 4096th key) after 8 lookups, and that sampling shows up in the first
  pass. Steady state is 3–11x faster. (Before the grids existed the warm path lookup on
  Wikipedia was 95–312 µs, 8–25x slower than libzim: a plain binary search over 27 M
  entries touches ~25 scattered pages.)
- **Cached-cluster reads** are 25 ns in zimz versus 12–25 µs through python-libzim: the
  Python binding allocates an Entry, an Item and a Blob per call, and libzim copies the
  blob. For large items (iFixit, wikispecies) both sides are bounded by touching pages.
- **Scans in path order** are decode-bound on archives whose clusters were filled in a
  different order (wikispecies: parity at ~58 MB/s on both sides, each item decoding a
  full cluster). iFixit and wikem have many uncompressed clusters, where libzim re-reads
  the offset table per item and zimz keeps it cached; hence 3–41x. A cluster-ordered
  iterator (libzim's `iterEfficient`) is the right tool for bulk extraction and is not
  implemented yet on either side of this harness.
- **Checksum** is MD5 over the file on both sides; ~900 MB/s either way.

## Changes made because of this benchmark

- `entry_by_title` does one lower-bound search instead of three (it used to compute a
  full prefix range first).
- Path and title lookups use lazily built sampled-key grids (`OpenConfig::lookup_bucket`,
  default 4096, built after 8 lookups), the technique libzim uses. Verified against the
  plain binary search on every key and its neighbours in `tests/synthetic.rs`.
