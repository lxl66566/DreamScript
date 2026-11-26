#!/usr/bin/env python3
"""Self-test for check_comment_refs.py. Run directly: python tools/test_check_comment_refs.py

Feeds the rule table a corpus of comment lines with their expected tier-1 ids
and probes the tier-2 budget, the allow-list, the comment-line scanner, and a
whole-tree scan. pytest-free, stdlib only, fast; exits nonzero on the first
failed assertion.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("check_comment_refs", HERE / "check_comment_refs.py")
ccr = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ccr)

# comment line -> the tier-1 ids it must raise, in rule order.
DENY_CASES = (
    ("//! the surface lives in docs/en/status.md", ["doc-path"]),
    ("// the design record holds the argument", ["doc-path"]),
    ("// the vocabulary is in the corpus README", ["doc-path"]),
    ("/// see conformance/README for the table", ["doc-path"]),
    ("// memory-arc owns this invariant", ["doc-path"]),
    ("// the rule is in pitfalls", ["doc-path"]),
    ("// see section 10 for the cycle rule", ["doc-number"]),
    ("// item 58 owns the fix", ["doc-number"]),
    ("// this is record 6.1's shape", ["doc-number"]),
    ("// the clause in §4", ["doc-number"]),
    ("// the uop set (BL-5), locked", ["rule-id"]),
    ("// C3 has exactly two re-assignable carriers", ["rule-id"]),
    ("// the M2 static story", ["rule-id"]),
    ("// stay the R13 abandoned corner", ["rule-id"]),
    ("// both (H5.5, E1) agree", ["rule-id"]),
    ("// (ruled 2026-11-25) it resolves inline", ["date", "ruling"]),
    ("/// since 2026-01-02 the ids are stable", ["date"]),
    ("// the rulings list is append-only", ["ruling"]),
    ("// todo 147 owns the remaining face", ["work-item"]),
    ("// slice 3 wires the loader", ["work-item"]),
    ("// fixes issue #1234 here", ["work-item"]),
    ("// round 4 reads the header alone", ["work-item"]),
    ("// see #42 for the reason", ["work-item"]),
    ("// the pass no longer refuses it", ["narration"]),
    ("// previously named the tail slot", ["narration"]),
    ("// now that the loader exists", ["narration"]),
    ("// this round reads ELF64 alone", ["narration"]),
    ("// the ownership round owns the refusals", ["narration"]),
    ("// the artifact round's wider words", ["narration"]),
    ("// the shape is superseded by the twins", ["narration"]),
    ("// residual gap: the handler set", ["narration"]),
)

# Lines that name code, data or an external spec and must stay clean.
ALLOW_CASES = (
    "// $t#1 names the type variable",
    "// the N1, N2 and N3 sizes differ",
    "// F64, I64 and U32 are width tokens",
    "// B_caller rides the callee save area",
    "// the round-trip encoding is byte-identical",
    "// the half rounds away from zero",
    "// a data record holds the four fields",
    "// install landed the slot in one store",
    "// until the OS answers would-block",
    "// A 32-bit ELF library: the reader parses ELF64 alone",
    "// the contract is three tiers",
    "// per the loader's own table",
    "// the two rounds of the poll loop",
)


def test_deny_cases() -> None:
    for line, expected in DENY_CASES:
        got = ccr.hits_in_line(line)
        assert got == expected, f"{line!r}: {got} != {expected}"
    print(f"ok: {len(DENY_CASES)} deny cases")


def test_allow_cases() -> None:
    for line in ALLOW_CASES:
        got = ccr.hits_in_line(line)
        assert got == [], f"{line!r} raised {got}"
    print(f"ok: {len(ALLOW_CASES)} clean cases")


def test_rule_order() -> None:
    # Rules report in declaration order, and a scan keeps the first one as the
    # line's single hit: one edit per line.
    line = "// todo 9 docs/en/status.md"
    assert ccr.hits_in_line(line) == ["doc-path", "work-item"], ccr.hits_in_line(line)
    print("ok: deny rules report in declaration order")


def test_budget_hits() -> None:
    assert ccr.budget_hits_in_line("// install landed the slot") == ["landed"]
    assert ccr.budget_hits_in_line("// the runtime slot is retired") == ["retired"]
    assert ccr.budget_hits_in_line("// per the loader") == ["per-the"]
    assert ccr.budget_hits_in_line("// the record holds it") == ["the-record"]
    assert ccr.budget_hits_in_line("// the contract is fixed") == ["the-contract"]
    assert ccr.budget_hits_in_line("// the next round pumps") == ["round"]
    assert ccr.budget_hits_in_line("// the round-trip encoding") == [], "round-trip is a shape"
    assert ccr.budget_hits_in_line("// round trip through the codec") == []
    assert ccr.budget_hits_in_line("// the half rounds away from zero") == ["round"]
    print("ok: tier-2 budget rules")


def test_allow_list() -> None:
    assert ccr._allowed("crates/ds-cli/src/doc.rs", "doc-path", "docs/en/")
    assert ccr._allowed("crates/ds-cli/tests/doc.rs", "doc-path", "docs/en/")
    assert not ccr._allowed("crates/ds-cli/src/lib.rs", "doc-path", "docs/en/")
    assert ccr._allowed("crates/ds-vm/src/tz.rs", "rule-id", "C11")
    assert not ccr._allowed("crates/ds-vm/src/tz.rs", "rule-id", "H5")
    assert not ccr._allowed("crates/ds-vm/src/other.rs", "rule-id", "C11")
    print("ok: allow-list")


def test_comment_lines() -> None:
    text = (
        "let x = 1; // docs/en/ is a trailing comment, not scanned\n"
        "//! docs/en/ in a doc comment is scanned\n"
        "let y = /* docs/en/ trailing block */ 2;\n"
        "/*\n"
        " the design record, in a block body\n"
        "*/\n"
        "let z = 3;\n"
    )
    got = list(ccr.comment_lines(text))
    assert [n for n, _ in got] == [2, 4, 5], got
    print("ok: comment-line scanner")


def test_read_budget(tmp: Path) -> None:
    path = tmp / "budget.txt"
    path.write_text("# a comment\n\nlanded 80  # trailing\nround 411\n", encoding="utf-8")
    budget = ccr.read_budget(path)
    assert budget == {"landed": 80, "round": 411}, budget
    bad = tmp / "bad.txt"
    bad.write_text("landed many\n", encoding="utf-8")
    try:
        ccr.read_budget(bad)
    except ValueError as err:
        assert "expected 'rule count'" in str(err), err
    else:
        raise AssertionError("a malformed budget line must raise")
    print("ok: budget parser")


def test_tree_scan(tmp: Path) -> None:
    root = tmp / "repo"
    (root / "crates" / "a" / "src").mkdir(parents=True)
    (root / "fuzz").mkdir()
    (root / "target" / "debug").mkdir(parents=True)
    (root / "crates" / "a" / "src" / "lib.rs").write_text(
        "//! see docs/en/status.md; the record owns it\n// the C4 rule\n", encoding="utf-8"
    )
    (root / "fuzz" / "gen.rs").write_text("// ruled 2026-11-25\n", encoding="utf-8")
    (root / "target" / "debug" / "gen.rs").write_text("// docs/en/ skipped\n", encoding="utf-8")
    hits, counts = ccr.scan(root)
    got = [(path, line, rule) for path, line, rule, _ in hits]
    assert got == [
        ("crates/a/src/lib.rs", 1, "doc-path"),
        ("crates/a/src/lib.rs", 2, "rule-id"),
        ("fuzz/gen.rs", 1, "date"),
    ], got
    assert counts["the-record"] == 1, counts
    print("ok: tree scan skips target/, covers crates/ and fuzz/")


def verdict(hits, counts, budget) -> int:
    with contextlib.redirect_stdout(io.StringIO()):
        return ccr.report(hits, counts, budget)


def test_report(tmp: Path) -> None:
    hits = [("crates/a/src/lib.rs", 1, "rule-id", "// the C4 rule")]
    counts = {rule: 0 for rule, _ in ccr.TIER2}
    budget = ccr.read_budget(HERE / "comment_refs_budget.txt")
    assert verdict([], counts, budget) == 0, "a clean tree passes"
    assert verdict(hits, counts, budget) == 1, "a tier-1 hit fails"
    counts["landed"] = budget["landed"] + 1
    assert verdict([], counts, budget) == 1, "a tier-2 rise fails"
    counts["landed"] = budget["landed"]
    assert verdict([], counts, budget) == 0, "a tier-2 count at its budget passes"
    missing = {rule: 0 for rule, _ in ccr.TIER2}
    del missing["round"]
    assert verdict([], counts, missing) == 1, "a rule without a budget fails"
    print("ok: report verdicts")


def main() -> None:
    ccr._configure_streams()
    test_deny_cases()
    test_allow_cases()
    test_rule_order()
    test_budget_hits()
    test_allow_list()
    test_comment_lines()
    with tempfile.TemporaryDirectory(prefix="check_comment_refs_selftest_") as td:
        tmp = Path(td)
        test_read_budget(tmp)
        test_tree_scan(tmp)
        test_report(tmp)
    print("self-test: all checks passed")


if __name__ == "__main__":
    main()
