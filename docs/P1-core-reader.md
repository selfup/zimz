# P1 — `zimz-core` reader: results (2026-09-23)

Scope: phases P0–P1 (bootstrap and the ZIM reader). Everything below was measured on this machine
(Apple Silicon, macOS, NVMe) against the local library in `~/zims`.

## What exists

- `crates/zimz-core`: header, MIME list, dirents (cached LRU), path binary search,
  namespace ranges, libzim-compatible path/title resolution, clusters (none / zstd / xz,
  extended offsets, streaming decode with no assumed compressed length, byte-budgeted LRU),
  blobs (zero-copy for uncompressed clusters), metadata + Counter/Tags/Illustration helpers,
  main page, title listings (`X/listing/titleOrdered/v1` → header list → none), embedded
  Xapian index location (`DirectAccess`), MD5 verification, integrity checks, split
  archives (`.zimaa…`), byte-range archives (libzim `FdInput` lists), embedded archives.
- Codecs behind features: `zstd-c`/`xz-c` (default, C libs) or `zstd-pure`/`xz-pure`
  (ruzstd, lzma-rust2). Both pass the full suite. ruzstd needed its window cap raised
  from 100 MB to 256 MiB for 128 MiB-window archives.
- `crates/zimz-cli`: `zimz info | ls | cat | check | dump-index`.
- Tests (see "Testing" below): unit tests in every module, fixture suites over the
  openZIM testing suite, behaviour tests on archives built in memory, a mutation
  (no-panic) suite, property tests, a local-library sweep and a python-libzim parity
  harness.

## Testing

| Suite | What it covers | Count | Time (C codecs) |
|---|---|---|---|
| Unit (`src/*`) | header validation (13 rejection cases), MIME list, dirent layouts incl. deprecated kinds and 70 KB paths, cluster offset rules, extended clusters, codec limits and garbage input, cache eviction, every `Source` type incl. temp-file split parts and ranges, metadata parsers | 52 | < 10 ms |
| `tests/fixtures.rs` | openZIM testing suite, 3 flavours: every item of every valid file, MD5, all integrity checks, title ordering and prefix ranges, split vs whole, embedded (+8 offset) and multi-range embedded, 63 invalid files rejected | 6 | 1 s |
| `tests/synthetic.rs` | archives built by `tests/common/builder.rs` (validated against python-libzim): path lookup and insertion points, namespace ranges, redirect chains and loops, main page both ways, metadata/Counter/article count, both title-index kinds and no index, `entry_by_path_compat` and `entry_by_title` rules, direct access, zstd/xz/extended clusters with many small clusters, cache byte budget, decompression cap, huge dirents, empty titles, checksum corruption/truncation, one crafted corruption per integrity check, out-of-range requests, illustration fallback, split archives from temp files | 25 (+1 ignored writer) | 30 ms |
| `tests/mutation.rs` | 600 random mutations (bit flips, truncation, zeroing, extreme field values, insertions, deletions) of each of 3 fixture flavours + new/old/zstd/extended/xz synthetic archives, full read path under `catch_unwind`; exhaustive single-bit flips of all 80 header bytes | 3 (≈5 000 corrupted archives) | 1 s |
| `tests/proptests.rs` | proptest, 400 cases each: header/dirent/MIME parsers and whole archives on random bytes never panic; dirent and Counter round trips; builder→reader round trip with random entries, both schemes | 7 | 0.6 s |
| `tests/parity.rs` | python-libzim manifest comparison via `uv run scripts/parity.py`; CI runs it on three fixtures, locally on 13 real archives so far | 13 archives | < 2 s each |
| `tests/local_library.rs` (opt-in) | every file in `ZIMZ_TEST_ZIM_DIR` | 51 files | 20 s |

Total in CI: 95 tests, about 4 s wall on the C-codec build; the pure-codec build runs the
same suites minus the encoder-dependent ones (87 tests).

Hardening applied while writing these: xz decoders now carry a 128 MiB memory limit
(liblzma `memlimit`, lzma-rust2 `new_mem_limit`), and ruzstd's window cap is 128 MiB
(libzstd's default). A corrupt header can no longer force a multi-GiB allocation.

Known limitation: lzma-rust2 initialises the full 64 MiB dictionary an xz cluster declares
on every decode, so a pure-Rust xz decode costs ~5 ms in release builds (~0.15 ms with
liblzma) and ~90 ms unoptimised. The C codecs stay the default, and
`[profile.dev.package.*]` keeps the decoders optimised in test builds so the suites stay fast.

## Acceptance results

| P1 acceptance criterion | Result |
|---|---|
| Testing-suite fixtures incl. corrupted ones, no panics | all pass; every invalid file is rejected at open or by `integrity::run` |
| Byte-identical vs python-libzim | 13 archives, ~9 900 sampled entries (paths, titles, redirect targets, sizes, MD5, MIME, title lookups): 0 mismatches. ZIM 5.0 (wikem, AoPS), 6.2 (ifixit, libretexts, zimgit, cdc), 6.3 (devdocs, mankier, gutenberg, wikispecies, youtube2zim, Wikipedia 202 samples) |
| `zimz info/ls/cat` on all local files | 57/57 |
| Old scheme + split files | wikem/AoPS (5.0) parity; split + range fixtures pass |
| Fuzz 1 h | **not run**: no nightly toolchain / cargo-fuzz on this machine. Targets are in `fuzz/`. |

## Timings (release build, `cargo run --release -p zimz-core --example bench`)

| Operation | Wikipedia en maxi (124 GB, 27.2 M entries) | wikem (42 MB, 8 060 entries) | Target (§6.9) |
|---|---|---|---|
| Open | 0.26 ms | 2.7 ms | < 50 ms |
| Entry by index (random, cold pages) | 86 µs | 0.8 µs | — |
| Path lookup (binary search, cold) | 261 µs | 2.8 µs | < 50 µs warm |
| Item read, cold zstd cluster (~2 MiB) | 0.76 ms | 0.16 ms | < 5 ms |
| Item read, warm cache | 99 ns | 32 ns | < 100 µs |
| Title prefix range | 319 µs | 2 µs | — |

Cold Wikipedia numbers are disk-bound page faults on a 124 GB mmap; warm-cache
lookups are microseconds. Local-library sweep (51 files, checksums for files < 256 MiB,
main page + 25 entries + title order per file): 20 s.

## Comparison with python-libzim

See `docs/bench-vs-python-libzim.md` (`uv run scripts/bench_compare.py`): warm lookups
3–40x faster than the reference reader, cluster decoding at parity (same libzstd),
cached reads two to three orders of magnitude faster. That work added the lookup grids
and simplified `entry_by_title`.

## Notes for P2

- `Archive::fulltext_index()` / `title_xapian_index()` return the byte range of the glass
  DB; `Archive::source().slice(..)` gives zero-copy access, and `Source` handles split
  archives, so the glass reader should take a `BlockSource` over these.
- `zimz dump-index` writes the raw glass blob for `xapian-delve` / `quest` oracles.
- `Archive::user_path` mirrors libzim's path semantics (namespace-prefixed for 5.0 files);
  search results should use it.
