#!/usr/bin/env python3
"""Regenerate docs/en/reference.md and docs/en/errors.md from `ds doc`.

The two checked-in documents are `ds doc` and `ds doc --errors`'s own
bytes: the ds-cli drift tests fail when the language surface or the
diagnostic registry moves without a regen. This helper is the one-step
regen a surface round ends with: it rebuilds the `ds` bin (`cargo run`
does it), captures both outputs, rewrites each file only when its bytes
changed, and prints a per-file added/removed line count for the review
step. The review itself reads `git diff docs/en` — a regen that moves
more than the round intended must be caught by a human, not the gate.

Usage:
  python tools/regen_docs.py

Stdlib only, Python 3.9+. Cross-platform: the binary rides `cargo run`,
so CARGO_TARGET_DIR and the executable suffix never matter here, and the
files are written as raw bytes to keep the line endings byte-stable.
"""

from __future__ import annotations

import difflib
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Each checked-in document with the `ds doc` arguments that produce it.
DOCS = [
    ("docs/en/reference.md", ()),
    ("docs/en/errors.md", ("--errors",)),
]


def dump(args: tuple[str, ...]) -> str:
    """One subcommand's stdout, decoded; a failure surfaces its stderr."""
    out = subprocess.run(
        ["cargo", "run", "--quiet", "-p", "ds-cli", "--", "doc", *args],
        cwd=ROOT,
        capture_output=True,
    )
    if out.returncode != 0:
        sys.stderr.write(out.stderr.decode(errors="replace"))
        raise SystemExit(f"regen_docs: ds doc {' '.join(args)} exited {out.returncode}")
    return out.stdout.decode()


def summarize(path: str, old: str, new: str) -> str:
    diff = difflib.unified_diff(
        old.splitlines(), new.splitlines(), fromfile=path, tofile=path, lineterm=""
    )
    lines = list(diff)
    added = sum(1 for line in lines if line.startswith("+") and not line.startswith("+++"))
    removed = sum(1 for line in lines if line.startswith("-") and not line.startswith("---"))
    return f"{path}: +{added} -{removed} lines"


def run() -> int:
    for path, args in DOCS:
        file = ROOT / path
        old = file.read_bytes().decode()
        new = dump(args)
        if new == old:
            print(f"{path}: unchanged")
            continue
        print(summarize(path, old, new))
        file.write_bytes(new.encode())
    return 0


if __name__ == "__main__":
    sys.exit(run())
