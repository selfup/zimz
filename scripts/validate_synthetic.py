#!/usr/bin/env python3
"""Validate the test builder's archives with python-libzim (the reference reader).

    cargo test -p zimz-core --test synthetic -- --ignored write_synthetic
    uv run scripts/validate_synthetic.py            # reads target/synthetic/*.zim

Every archive must open, verify its checksum, resolve its main page, expose the sample
metadata, find "Apple pie" by title, and let every entry's content be read. Exit code 1
on any failure, so CI can gate on it.
"""
import glob, sys
from pathlib import Path

from libzim.reader import Archive

ROOT = Path(__file__).resolve().parent.parent
EXPECTED = {
    "new": {"main": "Home", "title_path": "Apple_pie", "entries": 14, "new_scheme": True},
    "old": {"main": "A/Home", "title_path": "A/Apple_pie", "entries": 12, "new_scheme": False},
}


def validate(path: str) -> list[str]:
    problems = []
    name = Path(path).name
    exp = EXPECTED["new" if name.startswith("new") else "old"]
    a = Archive(path)
    if a.all_entry_count != exp["entries"]:
        problems.append(f"entry count {a.all_entry_count} != {exp['entries']}")
    if a.has_new_namespace_scheme != exp["new_scheme"]:
        problems.append(f"namespace scheme mismatch ({a.has_new_namespace_scheme})")
    if not a.check():
        problems.append("checksum verification failed")
    main = a.main_entry.get_item().path
    if main != exp["main"]:
        problems.append(f"main page {main!r} != {exp['main']!r}")
    hit = a.get_entry_by_title("Apple pie").path
    if hit != exp["title_path"]:
        problems.append(f"title lookup {hit!r} != {exp['title_path']!r}")
    if a.get_metadata("Title").decode() != "Sample":
        problems.append("Title metadata mismatch")
    total = 0
    for i in range(a.all_entry_count):
        e = a._get_entry_by_id(i)
        if e.is_redirect:
            e.get_item()  # follows the redirect
        else:
            total += len(bytes(e.get_item().content))
    if total == 0:
        problems.append("no content read")
    return problems


def main() -> int:
    files = sorted(glob.glob(str(ROOT / "target" / "synthetic" / "*.zim")))
    if not files:
        print("no archives in target/synthetic; run the ignored test first", file=sys.stderr)
        return 1
    failed = 0
    for f in files:
        try:
            problems = validate(f)
        except Exception as e:  # noqa: BLE001 - report and keep going
            problems = [f"exception: {e!r}"]
        status = "OK " if not problems else "FAIL"
        print(f"{status} {Path(f).name}" + ("" if not problems else ": " + "; ".join(problems)))
        failed += bool(problems)
    print(f"{len(files) - failed}/{len(files)} synthetic archives accepted by python-libzim")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
