#!/bin/sh
# Startup bench: cold-start medians for the ds CLI over three fixed
# stages plus the script-cache hit stage (M9 performance-gate prep).
# Modes:
#   startup.sh bench [--json] [-n runs] [metric...]
#       metric... filters the stages: floor trivial small-real warm.
# Stages:
#   floor       `ds --version` — the process + allocator + CLI-parse
#               floor; no compiler or VM code runs.
#   trivial     `ds run startup/trivial.ds` — three statements of pure
#               arithmetic plus one print: front-end + check + lower +
#               VM entry on an empty program graph.
#   small-real  `ds run startup/small-real.ds` — one list literal, a map
#               through the constructor + the set member, one reduce:
#               the same pipeline plus first-allocations cost.
#   warm        `ds run startup/trivial.ds` served from the script-mode
#               dsmod cache (a private DS_CACHE primed untimed) — the
#               mmap-hit path, so warm - floor is the artifact load +
#               install + execute cost.
# The cold stages force `--no-cache`: they never inherit or write a user
# cache, and their numbers stay comparable with the pre-cache suite.
# Adjacent deltas are the diagnostic (trivial - floor is pipeline entry
# cost; small-real - trivial is first-allocation cost).
#
# Numbers are meaningful ONLY under an exclusive lock with the machine
# otherwise idle (same discipline as run.sh bench: an exclusive `flock` on
# the machine-wide lock file). A single run is
# noise; medians under ~30 runs on a loaded machine are indicative
# only. Timings are printed, never committed — performance rounds
# record the medians in commit messages.
#
# Correctness: every timed run's `result:` line is checked against the
# fixture's .golden (floor checks its version line); any mismatch or
# nonzero exit fails loudly — a wrong-result benchmark is worse than
# none.
#
# The ds interpreter is the release CLI, resolved exactly like run.sh:
# DS_BIN wins, else <repo>/target/release/ds(.exe). Build it first with
#   CARGO_TARGET_DIR=<shared target> cargo build -p ds-cli --release

set -u

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
sdir="$here/startup"

# Per-invocation timeout guard: `timeout` when the platform has it
# (GNU coreutils ships it on the Linux runners and Git-for-Windows),
# else a passthrough — a hung interpreter then fails the invocation
# itself instead of wedging a bench round.
TIMEOUT_SECS=600
if command -v timeout >/dev/null 2>&1; then
    TIMEOUT() { timeout "$TIMEOUT_SECS" "$@"; }
else
    TIMEOUT() { "$@"; }
fi

# ---- arguments ---------------------------------------------------------------

mode=${1:-}
[ $# -gt 0 ] && shift

usage() {
    echo "usage: startup.sh bench [--json] [-n runs] [metric...]" >&2
    echo "  metrics: floor trivial small-real warm" >&2
    exit 2
}

runs=30
json=0
selected=""
while [ $# -gt 0 ]; do
    case $1 in
    -n)
        [ $# -ge 2 ] || usage
        runs=$2
        shift
        ;;
    --json) json=1 ;;
    --*) usage ;;
    -*) usage ;;
    *) selected="$selected $1" ;;
    esac
    shift
done
selected=${selected# }

[ "$mode" = bench ] || usage
case $runs in
'' | *[!0-9]*) usage ;;
esac
[ "$runs" -gt 0 ] || usage

# ---- ds binary ----------------------------------------------------------------
# DS_BIN wins, else the repo's shared target dir (release/ds.exe on
# Windows, release/ds elsewhere) — same resolution as run.sh.

ds_bin=""
if [ -n "${DS_BIN:-}" ]; then
    [ -x "$DS_BIN" ] && ds_bin=$DS_BIN
else
    for candidate in "$here/../target/release/ds.exe" "$here/../target/release/ds"; do
        if [ -x "$candidate" ]; then
            ds_bin=$candidate
            break
        fi
    done
fi
if [ -z "$ds_bin" ]; then
    echo "startup.sh: no ds binary (set DS_BIN or build: CARGO_TARGET_DIR=<target> cargo build -p ds-cli --release)" >&2
    exit 1
fi

all_metrics="floor trivial small-real warm"
metrics=""
for name in $all_metrics; do
    keep=0
    if [ -z "$selected" ]; then
        keep=1
    else
        for want in $selected; do
            [ "$want" = "$name" ] && keep=1
        done
    fi
    [ "$keep" = 1 ] && metrics="$metrics $name"
done
metrics=${metrics# }
if [ -z "$metrics" ]; then
    echo "startup.sh: no metric matches:$selected" >&2
    exit 2
fi

for name in $metrics; do
    case $name in
    trivial | small-real | warm)
        fixture=$name
        [ "$name" = warm ] && fixture=trivial
        [ -f "$sdir/$fixture.ds" ] || {
            echo "startup.sh: missing fixture $sdir/$fixture.ds" >&2
            exit 1
        }
        [ -s "$sdir/$fixture.golden" ] || {
            echo "startup.sh: missing golden $sdir/$fixture.golden" >&2
            exit 1
        }
        ;;
    esac
done

# ---- bench -------------------------------------------------------------------
# One untimed warmup per stage (fs/av caches), then N timed runs, each
# verified against the golden before its timing is kept.

outtmp=$(mktemp 2>/dev/null) || outtmp="/tmp/ds-startup-out.$$"
valstmp=$(mktemp 2>/dev/null) || valstmp="/tmp/ds-startup-vals.$$"
warm_cache=$(mktemp -d 2>/dev/null) || warm_cache="/tmp/ds-startup-warm.$$"
mkdir -p "$warm_cache"
trap 'rm -f "$outtmp" "$valstmp"; rm -rf "$warm_cache"' EXIT
trap 'exit 1' INT TERM

die() {
    echo "FAIL: $*" >&2
    exit 1
}

# Runs the stage once, stdout into $outtmp; caller checks status.
run_stage() {
    stage=$1
    case $stage in
    floor) TIMEOUT "$ds_bin" --version ;;
    warm) DS_CACHE=$warm_cache TIMEOUT "$ds_bin" run "$sdir/trivial.ds" ;;
    *) TIMEOUT "$ds_bin" run --no-cache "$sdir/$stage.ds" ;;
    esac >"$outtmp" 2>/dev/null
}

# Verifies the just-run stage's output line against its expectation.
check_stage() {
    stage=$1
    if [ "$stage" = floor ]; then
        out=$(sed -n 's/\r$//;1p' "$outtmp")
        case $out in
        ds\ *) return 0 ;;
        *) die "floor: expected a version line, got [$out]" ;;
        esac
    fi
    # the warm stage runs the trivial fixture through the cache
    [ "$stage" = warm ] && stage=trivial
    golden=$(sed -n 's/\r$//;1p' "$sdir/$stage.golden")
    out=$(sed 's/\r$//' "$outtmp" | grep '^result:' | tail -n 1)
    [ -n "$out" ] || die "$stage: no \`result:\` line on stdout (did the script run?)"
    [ "$out" = "$golden" ] || die "$stage: expected [$golden] got [$out]"
}

# `date +%s%N` prints nanoseconds on GNU date only; BSD date answers
# seconds plus a literal `N`. These numbers gate M9 perf records, so a
# non-digit answer refuses loudly instead of timing on a broken scale
# (the portable fallback would be `date +%s` whole seconds).
probe=$(date +%s%N)
case $probe in
'' | *[!0-9]*)
    echo "startup.sh: \`date +%s%N\` answered [$probe] — this date has no nanoseconds (BSD); the bench needs GNU date, or a rework over the \`date +%s\` fallback" >&2
    exit 1
    ;;
esac

if [ "$json" = 0 ]; then
    printf '# ds: %s (%s) runs=%s\n' "$ds_bin" "$(TIMEOUT "$ds_bin" --version 2>/dev/null)" "$runs"
    printf '%-12s %5s %11s %11s %11s\n' METRIC RUNS MEDIAN_MS MIN_MS P95_MS
fi

for metric in $metrics; do
    run_stage "$metric" || die "$metric: warmup run exited $?"
    # the warm stage's first run primes its private cache; the warmup
    # above is that prime, so every timed run takes the hit path
    check_stage "$metric"
    : >"$valstmp"
    i=0
    while [ "$i" -lt "$runs" ]; do
        t0=$(date +%s%N)
        run_stage "$metric"
        status=$?
        t1=$(date +%s%N)
        [ "$status" -eq 0 ] || die "$metric: run $((i + 1)) exited $status"
        check_stage "$metric"
        printf '%s\n' $((t1 - t0)) >>"$valstmp"
        i=$((i + 1))
    done
    sort -n "$valstmp" | awk -v metric="$metric" -v runs="$runs" -v json="$json" '
        { a[NR] = $1 }
        END {
            min = a[1]
            if (NR % 2) med = a[(NR + 1) / 2]
            else med = (a[NR / 2] + a[NR / 2 + 1]) / 2
            r = int(NR * 0.95)
            if (r < NR * 0.95) r += 1
            if (json)
                printf "{\"metric\":\"%s\",\"runs\":%d,\"median_ms\":%.3f,\"min_ms\":%.3f,\"p95_ms\":%.3f}\n", metric, runs, med / 1000000, min / 1000000, a[r] / 1000000
            else
                printf "%-12s %5d %11.3f %11.3f %11.3f\n", metric, runs, med / 1000000, min / 1000000, a[r] / 1000000
        }'
done
