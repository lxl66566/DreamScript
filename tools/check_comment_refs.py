#!/usr/bin/env python3
"""Deny code comments that depend on an external document.

A comment states current behavior. It must not point at a document, a doc
section or item number, a decision-rule ID, a date, a ruling attribution, a
work item, or a phase of the project's history: every such token rots when the
document moves, and the reader of the code cannot resolve it.

Two tiers:

  tier 1 - deny, must stay at zero. A hit fails with ``file:line: rule: text``.
  tier 2 - budget, frozen in ``tools/comment_refs_budget.txt`` as ``rule count``
           lines. These words also carry a correct code meaning, so a count may
           fall but never rise.

The scanner reads full-line comments only: lines whose first non-space text is
``//`` or ``/*``, plus the bodies of ``/* */`` blocks. A trailing comment after
code and any string literal are out of reach by design.

Usage:
  python tools/check_comment_refs.py [--root DIR] [--budget FILE]
  python tools/check_comment_refs.py --self-test

Stdlib only, Python 3.9+.
"""

from __future__ import annotations

import argparse
import re
import runpy
import sys
from pathlib import Path

SCAN_ROOTS = ("crates", "fuzz", "benchmarks")
SKIPPED_PARTS = ("target",)

TIER1 = (
    (
        "doc-path",
        r"docs/en/|design-decisions|implementation-plan|status\.md|todo\.md"
        r"|pitfalls|memory-arc|tutorial\.md|(?:conformance/)?README|\bdesign record\b",
    ),
    (
        "doc-number",
        r"\bsections? \d|\bitems? \d+\b|§|\brecord \d+\.\d+",
    ),
    (
        "rule-id",
        r"(?<![\w.$])(?:BL-[1-9]|[BCDEFGH][0-9](?:\.[0-9])?|M[0-9]|R[0-9]{1,2})(?![\w])",
    ),
    ("date", r"\b20\d\d-\d\d-\d\d\b"),
    ("ruling", r"\bruled\b|\brulings?\b"),
    (
        "work-item",
        r"\btodo \d+\b|\bslice \d+\b|\bissue #?\d+\b|\bround \d+\b|(?<![$\w])#[0-9]+",
    ),
    (
        "narration",
        r"\bno longer\b|\bpreviously\b|\bnow that\b|\bthis rounds?\b"
        r"|\bthe (?:[a-z-]+ )?round's\b|\bownership round\b"
        r"|\bsuperseded\b|\bresiduals?\b",
    ),
)

TIER2 = (
    ("landed", r"\blanded\b"),
    ("retired", r"\bretire[sd]\b|\bretiring\b"),
    ("per-the", r"\bper the\b"),
    ("the-record", r"\bthe record\b"),
    ("the-contract", r"\bthe contract\b"),
    # round-trip and round trip are data-shape words, not project phases.
    ("round", r"\brounds?\b(?!-trips?\b|\s+trip\b)"),
)

# A match the rule would deny but that names something real. path is
# repo-relative, match is None (any text) or a substring the comment line must
# carry.
ALLOWED = (
    {
        "path": "crates/ds-cli/src/doc.rs",
        "rule": "doc-path",
        "match": None,
        "reason": "docs/en/reference.md and docs/en/errors.md are the dumper's own output paths",
    },
    {
        "path": "crates/ds-cli/tests/doc.rs",
        "rule": "doc-path",
        "match": None,
        "reason": "the drift test reads the dumper's output files from disk",
    },
    {
        "path": "crates/ds-vm/src/tz.rs",
        "rule": "rule-id",
        "match": "C11",
        "reason": "C standard Annex K argument order, an external spec",
    },
    {
        "path": "crates/ds-diag/src/lib.rs",
        "rule": "narration",
        "match": "no longer usable",
        "reason": "the DS3001 message text, quoted in a doctest",
    },
    {
        "path": "crates/ds-diag/src/render.rs",
        "rule": "narration",
        "match": "no longer usable",
        "reason": "the DS3001 message text in the rendered-shape example",
    },
)

TIER1_RE = tuple((rule, re.compile(rx)) for rule, rx in TIER1)
TIER2_RE = tuple((rule, re.compile(rx)) for rule, rx in TIER2)


def _configure_streams() -> None:
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            try:
                stream.reconfigure(encoding="utf-8", errors="replace")
            except (OSError, ValueError):
                pass


def comment_lines(text: str):
    """Yield (lineno, line) for full-line comments and /* */ bodies."""
    depth = 0
    for lineno, line in enumerate(text.splitlines(), 1):
        stripped = line.strip()
        if depth:
            if stripped != "*/":
                yield lineno, line
            depth = max(0, depth + stripped.count("/*") - stripped.count("*/"))
            continue
        if stripped.startswith("//"):
            yield lineno, line
        elif stripped.startswith("/*"):
            yield lineno, line
            depth = max(0, stripped.count("/*") - stripped.count("*/"))


def hits_in_line(text: str):
    """Tier-1 rule ids matched in one comment line, in rule order."""
    return [rule for rule, rx in TIER1_RE if rx.search(text)]


def budget_hits_in_line(text: str):
    """Tier-2 rule ids matched in one comment line, in rule order."""
    return [rule for rule, rx in TIER2_RE if rx.search(text)]


def _allowed(path: str, rule: str, text: str) -> bool:
    for entry in ALLOWED:
        if entry["path"] != path or entry["rule"] != rule:
            continue
        if entry["match"] is None or entry["match"] in text:
            return True
    return False


def source_files(root: Path):
    for name in SCAN_ROOTS:
        top = root / name
        if not top.is_dir():
            continue
        for path in sorted(top.rglob("*.rs")):
            if any(part in SKIPPED_PARTS for part in path.parts):
                continue
            yield path


def scan(root: Path):
    """Return (tier1 hits, tier2 counts). A hit is (path, line, rule, text)."""
    hits = []
    counts = {rule: 0 for rule, _ in TIER2}
    for path in source_files(root):
        rel = path.relative_to(root).as_posix()
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError as err:
            print(f"check_comment_refs: cannot read {rel}: {err}", file=sys.stderr)
            continue
        for lineno, line in comment_lines(text):
            for rule, rx in TIER1_RE:
                match = rx.search(line)
                if match and not _allowed(rel, rule, line):
                    hits.append((rel, lineno, rule, line.strip()))
                    break
            for rule in budget_hits_in_line(line):
                counts[rule] += 1
    return hits, counts


def read_budget(path: Path):
    """Parse ``rule count`` lines; ``#`` starts a comment."""
    budget = {}
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        body = line.split("#", 1)[0].strip()
        if not body:
            continue
        parts = body.split()
        if len(parts) != 2 or not parts[1].isdigit():
            raise ValueError(f"{path}:{lineno}: expected 'rule count', got '{line.strip()}'")
        budget[parts[0]] = int(parts[1])
    return budget


def report(hits, counts, budget) -> int:
    for path, lineno, rule, text in hits:
        print(f"{path}:{lineno}: {rule}: {text}")
    bad = bool(hits)
    print(f"tier 1: {len(hits)} hits (deny, must be zero)")
    for rule, _ in TIER2:
        have = counts[rule]
        frozen = budget.get(rule)
        if frozen is None:
            print(f"tier 2: {rule}: {have} (no frozen budget)")
            bad = True
            continue
        state = "over budget" if have > frozen else "ok"
        print(f"tier 2: {rule}: {have} (frozen {frozen}, {state})")
        if have > frozen:
            print(
                f"comment_refs: '{rule}' rose from {frozen} to {have}; "
                "a comment must state current behavior without the phase word",
                file=sys.stderr,
            )
            bad = True
    return 1 if bad else 0


def run(argv=None) -> int:
    parser = argparse.ArgumentParser(
        description="Deny comment text that depends on an external document."
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root to scan (default: this script's parent)",
    )
    parser.add_argument(
        "--budget",
        type=Path,
        default=None,
        help="tier-2 budget file (default: tools/comment_refs_budget.txt)",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run the self-test (tools/test_check_comment_refs.py) and exit",
    )
    args = parser.parse_args(argv)
    if args.self_test:
        runpy.run_path(
            str(Path(__file__).resolve().parent / "test_check_comment_refs.py"),
            run_name="__main__",
        )
        return 0
    _configure_streams()
    root = args.root.resolve()
    budget_path = args.budget or Path(__file__).resolve().parent / "comment_refs_budget.txt"
    try:
        budget = read_budget(budget_path)
    except (OSError, ValueError) as err:
        print(f"check_comment_refs: {err}", file=sys.stderr)
        return 1
    hits, counts = scan(root)
    return report(hits, counts, budget)


if __name__ == "__main__":
    sys.exit(run())
