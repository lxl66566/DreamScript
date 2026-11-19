#!/usr/bin/env python3
"""Stale-artifact GC for per-unit cargo target directories.

Nightly cargo's per-unit layout never removes a replaced unit directory, so
target/debug/build/<pkg>/<hash>/ corpses accumulate forever (a busy lane piles
gigabytes per week). This tool selects unit directories whose fingerprint/
directory mtime is older than a cutoff (default 7 days) and deletes them. The
fingerprint lives inside the unit directory, so a deletion can only force a
rebuild of that unit, never a wrong build.

Usage:
  python tools/gc_targets.py [--days N] [--apply] [--incremental] TARGET [TARGET ...]

Dry-run by default: prints the reclaimable bytes and stale unit-dir count per
target directory plus a total. --apply performs the deletion. --incremental
additionally clears each target directory's debug/incremental* (stock cargo's
incremental plus kache's incremental.kache-auto) and debug/tmp (the round-close
sweep). --self-test runs tools/test_gc_targets.py.

Safety rules: only entries below a resolved target root are ever touched;
symlinks and Windows junctions/reparse points are never traversed (a link node
inside a deleted tree is unlinked as a node, its target untouched); entries
that cannot be stat'd are skipped with a warning. Stdlib only, Python 3.9+.
"""

from __future__ import annotations

import argparse
import os
import runpy
import stat as stat_mod
import sys
import time
from pathlib import Path

SECONDS_PER_DAY = 86400.0
# FILE_ATTRIBUTE_REPARSE_POINT from the Windows st_file_attributes flags.
REPARSE_POINT = 0x400


def _configure_streams() -> None:
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            try:
                stream.reconfigure(encoding="utf-8", errors="replace")
            except (OSError, ValueError):
                pass


def _stat_is_link(st: os.stat_result) -> bool:
    """True for symlinks and Windows junctions/reparse points."""
    return stat_mod.S_ISLNK(st.st_mode) or bool(
        getattr(st, "st_file_attributes", 0) & REPARSE_POINT
    )


def _lstat(path: Path):
    try:
        return os.lstat(path)
    except OSError:
        return None


def _human(n: float) -> str:
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if n < 1024.0 or unit == "TiB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.2f} {unit}"
        n /= 1024.0
    return f"{n:.2f} TiB"


def _tree_size(top: Path, warnings: list) -> int:
    """Sum of file lstat sizes under top; never follows links, counts nothing for them."""
    total = 0
    stack = [top]
    while stack:
        d = stack.pop()
        try:
            with os.scandir(d) as it:
                for entry in it:
                    try:
                        st = entry.stat(follow_symlinks=False)
                    except OSError as err:
                        warnings.append(f"cannot stat {entry.path}: {err}")
                        continue
                    if _stat_is_link(st):
                        continue
                    if stat_mod.S_ISDIR(st.st_mode):
                        stack.append(Path(entry.path))
                    else:
                        total += st.st_size
        except OSError as err:
            warnings.append(f"cannot read {d}: {err}")
    return total


def _remove_link_node(path: Path, warnings: list) -> bool:
    """Remove a link/junction node itself, never its target."""
    try:
        os.unlink(path)
        return True
    except OSError:
        pass
    try:
        # A Windows directory symlink or junction: rmdir removes the reparse
        # point alone and leaves the target's contents alone.
        os.rmdir(path)
        return True
    except OSError as err:
        warnings.append(f"cannot remove link {path}: {err}")
        return False


def _rmtree_no_follow(top: Path, warnings: list) -> bool:
    """Delete the real directory tree top without following any link."""
    subdirs: list = []
    stack = [top]
    ok = True
    while stack:
        d = stack.pop()
        try:
            with os.scandir(d) as it:
                for entry in it:
                    p = Path(entry.path)
                    try:
                        st = entry.stat(follow_symlinks=False)
                    except OSError as err:
                        warnings.append(f"cannot stat {entry.path}: {err}")
                        ok = False
                        continue
                    if _stat_is_link(st):
                        ok = _remove_link_node(p, warnings) and ok
                    elif stat_mod.S_ISDIR(st.st_mode):
                        stack.append(p)
                        subdirs.append(p)
                    else:
                        try:
                            os.unlink(p)
                        except OSError as err:
                            warnings.append(f"cannot delete {p}: {err}")
                            ok = False
        except OSError as err:
            warnings.append(f"cannot read {d}: {err}")
            ok = False
    # subdirs appends a parent before its children, so the reverse order
    # removes children first.
    for d in reversed(subdirs):
        try:
            os.rmdir(d)
        except OSError as err:
            warnings.append(f"cannot remove {d}: {err}")
            ok = False
    try:
        os.rmdir(top)
    except OSError as err:
        warnings.append(f"cannot remove {top}: {err}")
        ok = False
    return ok


class TargetReport:
    def __init__(self, root: Path):
        self.root = root
        self.stale_units = 0
        self.stale_bytes = 0
        self.stale_dirs = []  # selected unit dirs (Path), deleted by sweep()
        self.unrecognized = 0
        self.incremental_bytes = 0
        self.incremental_dirs = []  # selected incremental/tmp dirs, --incremental only
        self.deleted_units = 0
        self.deleted_incremental = 0
        self.failed = 0
        self.warnings = []


def _within(root: Path, path: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def collect(root: Path, cutoff: float, incremental: bool) -> TargetReport:
    report = TargetReport(root=root)
    build = root / "debug" / "build"
    if not build.is_dir():
        report.warnings.append(f"no debug/build under {root}")
        return report
    try:
        with os.scandir(build) as it:
            pkg_entries = sorted(it, key=lambda e: e.name)
    except OSError as err:
        report.warnings.append(f"cannot read {build}: {err}")
        return report
    for pkg_entry in pkg_entries:
        try:
            pkg_st = pkg_entry.stat(follow_symlinks=False)
        except OSError as err:
            report.warnings.append(f"cannot stat {pkg_entry.path}: {err}")
            continue
        if not stat_mod.S_ISDIR(pkg_st.st_mode) or _stat_is_link(pkg_st):
            continue
        try:
            with os.scandir(pkg_entry.path) as it:
                unit_entries = sorted(it, key=lambda e: e.name)
        except OSError as err:
            report.warnings.append(f"cannot read {pkg_entry.path}: {err}")
            continue
        for unit_entry in unit_entries:
            unit = Path(unit_entry.path)
            if not _within(root, unit):
                continue
            try:
                unit_st = unit_entry.stat(follow_symlinks=False)
            except OSError as err:
                report.warnings.append(f"cannot stat {unit}: {err}")
                continue
            if not stat_mod.S_ISDIR(unit_st.st_mode) or _stat_is_link(unit_st):
                continue
            fp_st = _lstat(unit / "fingerprint")
            if fp_st is None or not stat_mod.S_ISDIR(fp_st.st_mode) or _stat_is_link(fp_st):
                report.unrecognized += 1
                continue
            if fp_st.st_mtime < cutoff:
                report.stale_units += 1
                report.stale_dirs.append(unit)
                report.stale_bytes += _tree_size(unit, report.warnings)
    if incremental:
        # incremental* covers stock cargo's debug/incremental and kache's
        # renamed debug/incremental.kache-auto; tmp is cargo's scratch dir.
        candidates = []
        try:
            with os.scandir(root / "debug") as it:
                for entry in it:
                    if entry.name == "tmp" or entry.name.startswith("incremental"):
                        candidates.append(Path(entry.path))
        except OSError as err:
            report.warnings.append(f"cannot read {root / 'debug'}: {err}")
        for d in sorted(candidates):
            st = _lstat(d)
            if st is None or not stat_mod.S_ISDIR(st.st_mode) or _stat_is_link(st):
                if st is not None:
                    report.warnings.append(f"skipping {d}: not a real directory")
                continue
            report.incremental_dirs.append(d)
            report.incremental_bytes += _tree_size(d, report.warnings)
    return report


def sweep(report: TargetReport) -> None:
    """Apply the deletions a collect() pass selected."""
    root = report.root
    for unit in report.stale_dirs:
        st = _lstat(unit)
        if st is None:
            report.warnings.append(f"already gone: {unit}")
            report.failed += 1
            continue
        if _stat_is_link(st) or not _within(root, unit):
            report.warnings.append(f"refusing to delete link or escape: {unit}")
            report.failed += 1
            continue
        if _rmtree_no_follow(unit, report.warnings):
            report.deleted_units += 1
        else:
            report.failed += 1
    for d in report.incremental_dirs:
        st = _lstat(d)
        if st is None:
            continue
        if _stat_is_link(st) or not _within(root, d):
            report.warnings.append(f"refusing to delete link or escape: {d}")
            report.failed += 1
            continue
        if _rmtree_no_follow(d, report.warnings):
            report.deleted_incremental += 1
        else:
            report.failed += 1


def _print_reports(reports, apply: bool, days: float, cutoff: float) -> None:
    mode = "apply" if apply else "dry-run (pass --apply to delete)"
    when = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(cutoff))
    print(f"gc_targets [{mode}] cutoff {days:g} days (fingerprint mtime before {when})")
    total_units = 0
    total_bytes = 0
    total_inc = 0
    for r in reports:
        line = f"  {r.root}: {r.stale_units} stale unit dirs, {_human(r.stale_bytes)}"
        if r.incremental_dirs:
            line += f"; incremental+tmp {_human(r.incremental_bytes)}"
        if r.unrecognized:
            line += f"; {r.unrecognized} dirs without fingerprint left alone"
        if apply:
            line += (
                f" -> deleted {r.deleted_units} unit dirs"
                f" (+{r.deleted_incremental} incremental/tmp)"
                if r.incremental_dirs
                else f" -> deleted {r.deleted_units} unit dirs"
            )
            if r.failed:
                line += f", {r.failed} FAILED"
        print(line)
        total_units += r.stale_units
        total_bytes += r.stale_bytes
        total_inc += r.incremental_bytes
    grand = f"total: {total_units} unit dirs, {_human(total_bytes)}"
    if any(r.incremental_dirs for r in reports):
        grand += f" (+{_human(total_inc)} incremental/tmp)"
    print(grand)
    for r in reports:
        for w in r.warnings:
            print(f"warning [{r.root}]: {w}")


def run(argv=None) -> int:
    parser = argparse.ArgumentParser(
        description="Delete stale per-unit directories under cargo target dirs."
    )
    parser.add_argument(
        "targets",
        nargs="*",
        type=Path,
        help="one or more cargo target directories (the dir containing debug/)",
    )
    parser.add_argument(
        "--days",
        type=float,
        default=7.0,
        help="cutoff age in days for a unit dir's fingerprint/ mtime (default: 7)",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="perform the deletion (default is a dry-run)",
    )
    parser.add_argument(
        "--incremental",
        action="store_true",
        help="also clear each target dir's debug/incremental* and debug/tmp",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run the self-test (tools/test_gc_targets.py) and exit",
    )
    args = parser.parse_args(argv)
    if args.self_test:
        runpy.run_path(
            str(Path(__file__).resolve().parent / "test_gc_targets.py"),
            run_name="__main__",
        )
        return 0
    if not args.targets:
        parser.error("pass at least one target directory")
    if args.days < 0:
        parser.error("--days must be >= 0")
    _configure_streams()
    cutoff = time.time() - args.days * SECONDS_PER_DAY
    reports = []
    bad = 0
    for target in args.targets:
        try:
            root = target.expanduser().resolve()
        except OSError as err:
            print(f"gc_targets: cannot resolve {target}: {err}", file=sys.stderr)
            bad += 1
            continue
        if not root.is_dir():
            print(f"gc_targets: not a directory: {root}", file=sys.stderr)
            bad += 1
            continue
        reports.append(collect(root, cutoff, args.incremental))
    if not reports:
        return 1
    if args.apply:
        for r in reports:
            sweep(r)
    _print_reports(reports, args.apply, args.days, cutoff)
    return 1 if bad or any(r.failed for r in reports) else 0


if __name__ == "__main__":
    sys.exit(run())
