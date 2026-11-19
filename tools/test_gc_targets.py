#!/usr/bin/env python3
"""Self-test for gc_targets.py. Run directly: python tools/test_gc_targets.py

Fabricates a fake cargo target tree in a temp dir (old and fresh unit dirs, a
unit dir without a fingerprint, incremental/tmp content, a sibling lane, and
escape links) and asserts the selection and deletion behavior. pytest-free,
stdlib only, fast; exits nonzero on the first failed assertion.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("gc_targets", HERE / "gc_targets.py")
gc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gc)

DAY = 86400.0


def make_unit(build: Path, pkg: str, name: str, age_days: float) -> Path:
    unit = build / pkg / name
    (unit / "fingerprint").mkdir(parents=True, exist_ok=True)
    (unit / "fingerprint" / "f").write_bytes(b"abc")
    (unit / "out").mkdir()
    (unit / "out" / "x").write_bytes(b"0123456789")
    when = time.time() - age_days * DAY
    os.utime(unit / "fingerprint", (when, when))
    return unit


def make_tree(lane_root: Path) -> Path:
    """lane_root/<lane>/target with one stale unit, one fresh, one unrecognized."""
    target = lane_root / "target"
    build = target / "debug" / "build"
    make_unit(build, "pkg-a", "0000old", 10.0)
    make_unit(build, "pkg-a", "1111new", 0.0)
    nofp = build / "pkg-b" / "2222nofp" / "out"
    nofp.mkdir(parents=True)
    (nofp / "x").write_bytes(b"0123456789")
    (target / "debug" / "incremental" / "pkg").mkdir(parents=True)
    (target / "debug" / "incremental" / "pkg" / "incr.bin").write_bytes(b"12345")
    (target / "debug" / "tmp").mkdir(parents=True)
    (target / "debug" / "tmp" / "t.bin").write_bytes(b"1234")
    return target


def run_quiet(argv):
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        rc = gc.run(argv)
    return rc, buf.getvalue()


def test_selection_and_dry_run(tmp: Path) -> None:
    target = make_tree(tmp / "lane")
    cutoff = time.time() - 7.0 * DAY
    report = gc.collect(target.resolve(), cutoff, incremental=True)
    assert report.stale_units == 1, report.stale_units
    assert report.stale_bytes == 13, report.stale_bytes  # 3 fingerprint + 10 out
    assert report.unrecognized == 1, report.unrecognized
    assert report.incremental_bytes == 9, report.incremental_bytes
    rc, out = run_quiet(["--incremental", str(target)])
    assert rc == 0, rc
    assert "dry-run" in out and "1 stale unit dirs, 13 B" in out, out
    assert "13 B" in out and "9 B" in out, out
    assert (target / "debug" / "build" / "pkg-a" / "0000old").is_dir()
    assert (target / "debug" / "incremental" / "pkg" / "incr.bin").exists()
    assert (target / "debug" / "tmp" / "t.bin").exists()
    print("ok: selection and dry-run")


def test_apply(tmp: Path) -> None:
    target = make_tree(tmp / "lane")
    sibling = make_tree(tmp / "lane2")
    rc, out = run_quiet(["--apply", str(target)])
    assert rc == 0, (rc, out)
    assert "deleted 1 unit dirs" in out, out
    assert not (target / "debug" / "build" / "pkg-a" / "0000old").exists()
    assert (target / "debug" / "build" / "pkg-a" / "1111new").is_dir()
    assert (target / "debug" / "build" / "pkg-b" / "2222nofp").is_dir()
    # No --incremental: both stay.
    assert (target / "debug" / "incremental" / "pkg" / "incr.bin").exists()
    assert (target / "debug" / "tmp" / "t.bin").exists()
    # A sibling target dir passed nowhere is never touched.
    assert (sibling / "debug" / "build" / "pkg-a" / "0000old").is_dir()
    print("ok: apply deletes stale units only, keeps the sibling lane")


def test_apply_incremental(tmp: Path) -> None:
    target = make_tree(tmp / "lane")
    rc, out = run_quiet(["--apply", "--incremental", str(target)])
    assert rc == 0, (rc, out)
    assert "deleted 1 unit dirs (+2 incremental/tmp)" in out, out
    assert not (target / "debug" / "incremental").exists()
    assert not (target / "debug" / "tmp").exists()
    assert not (target / "debug" / "build" / "pkg-a" / "0000old").exists()
    assert (target / "debug" / "build" / "pkg-a" / "1111new").is_dir()
    print("ok: --incremental clears incremental and tmp")


def test_days_override(tmp: Path) -> None:
    target = make_tree(tmp / "lane")
    rc, out = run_quiet(["--days", "30", "--apply", str(target)])
    assert rc == 0, (rc, out)
    assert (target / "debug" / "build" / "pkg-a" / "0000old").is_dir()
    rc, out = run_quiet(["--days", "0", "--apply", str(target)])
    assert rc == 0, (rc, out)
    assert not (target / "debug" / "build" / "pkg-a" / "0000old").exists()
    assert not (target / "debug" / "build" / "pkg-a" / "1111new").exists()
    print("ok: --days cutoff overrides (30 keeps, 0 sweeps all)")


def test_escape_links(tmp: Path) -> None:
    outside = tmp / "outside"
    outside.mkdir()
    secret = outside / "secret.txt"
    secret.write_bytes(b"precious")
    target = make_tree(tmp / "lane")
    old = target / "debug" / "build" / "pkg-a" / "0000old"
    made_symlink = made_junction = False
    try:
        os.symlink(outside, old / "escape", target_is_directory=True)
        made_symlink = True
    except (OSError, NotImplementedError):
        print("note: symlink creation unavailable, skipping symlink case")
    if os.name == "nt" and hasattr(sys, "getwindowsversion"):
        try:
            import _winapi

            _winapi.CreateJunction(str(outside), str(old / "junc"))
            made_junction = True
        except (OSError, AttributeError, ImportError):
            print("note: junction creation unavailable, skipping junction case")
    size = gc._tree_size(old, [])
    assert size == 13, size  # links count nothing, never traverse
    rc, out = run_quiet(["--apply", str(target)])
    assert rc == 0, (rc, out)
    assert not old.exists(), "stale unit with links inside must be removed"
    assert secret.exists() and secret.read_bytes() == b"precious"
    if made_symlink or made_junction:
        print("ok: escape links removed as nodes, target contents intact")
    else:
        print("ok: escape links (none creatable; size and apply paths exercised)")


def test_bad_target(tmp: Path) -> None:
    rc, out = run_quiet(["--apply", str(tmp / "missing")])
    assert rc == 1, rc
    empty = tmp / "empty"
    empty.mkdir()
    rc, out = run_quiet([str(empty)])
    assert rc == 0, (rc, out)  # no debug/build: warn, not fail
    assert "no debug/build" in out, out
    print("ok: missing target fails, empty target warns")


def main() -> None:
    gc._configure_streams()
    for test in (
        test_selection_and_dry_run,
        test_apply,
        test_apply_incremental,
        test_days_override,
        test_escape_links,
        test_bad_target,
    ):
        with tempfile.TemporaryDirectory(prefix="gc_targets_selftest_") as td:
            test(Path(td))
    print("self-test: all checks passed")


if __name__ == "__main__":
    main()
