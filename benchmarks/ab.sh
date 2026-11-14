#!/bin/sh
# Interleaved A/B wall-clock timing for two ds binaries over the kernels.
#   ab.sh <binA> <binB> [rounds] [kernel...]
# Runs one kernel at a time, alternating A,B,A,B... for `rounds` per side
# (default 10), then prints per-kernel medians and the delta. One invocation
# is one sample pair set; run under an exclusive flock with the machine idle
# (same discipline as run.sh bench). Numbers are never committed. Both
# binaries run the cold path (`--no-cache`): neither side inherits or
# writes a user cache, and the pair stays comparable with run.sh — the
# script-cache key already separates the two builds, so an A/B of a
# lowering change measures two different bytecodes, never one twice.

set -u

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
kdir="$here/kernels"

binA=${1:?usage: ab.sh <binA> <binB> [rounds] [kernel...]}
binB=${2:?usage: ab.sh <binA> <binB> [rounds] [kernel...]}
rounds=${3:-10}
shift 3 2>/dev/null || shift $#

selected=${*:-}
all_kernels=$(ls "$kdir"/*.ds 2>/dev/null | sed 's|.*/||; s|\.ds$||' | sort)
kernels=""
for name in $all_kernels; do
    keep=0
    if [ -z "$selected" ]; then
        keep=1
    else
        for want in $selected; do
            [ "$want" = "$name" ] && keep=1
        done
    fi
    [ "$keep" = 1 ] && kernels="$kernels $name"
done
kernels=${kernels# }

times_bin() {
    bin=$1
    file=$2
    t0=$(date +%s%N)
    "$bin" run --no-cache "$file" >/dev/null 2>&1
    status=$?
    t1=$(date +%s%N)
    if [ "$status" -ne 0 ]; then
        printf 'FAIL'
    else
        printf '%s' $(((t1 - t0) / 1000000))
    fi
}

median_of() {
    # one integer per line on stdin (already FAIL-free), prints the median
    sort -n | awk '{a[NR]=$1} END {printf "%s", a[int((NR + 1) / 2)]}'
}

printf '%-12s %10s %10s %8s\n' KERNEL A_MS B_MS DELTA
for name in $kernels; do
    file="$kdir/$name.ds"
    : >"${TMPDIR:-/tmp}/ab_A.$$" 2>/dev/null || : >/tmp/ab_A.$$
    tmpA=$(ls "${TMPDIR:-/tmp}/ab_A.$$" 2>/dev/null || printf '/tmp/ab_A.%s' "$$")
    : >"${TMPDIR:-/tmp}/ab_B.$$" 2>/dev/null || : >/tmp/ab_B.$$
    tmpB=$(ls "${TMPDIR:-/tmp}/ab_B.$$" 2>/dev/null || printf '/tmp/ab_B.%s' "$$")
    r=0
    while [ "$r" -lt "$rounds" ]; do
        ta=$(times_bin "$binA" "$file")
        tb=$(times_bin "$binB" "$file")
        printf '%s\n' "$ta" >>"$tmpA"
        printf '%s\n' "$tb" >>"$tmpB"
        r=$((r + 1))
    done
    ma=$(median_of <"$tmpA")
    mb=$(median_of <"$tmpB")
    if [ "$ma" = FAIL ] || [ "$mb" = FAIL ]; then
        printf '%-12s %10s %10s %8s\n' "$name" "$ma" "$mb" RUNFAIL
    else
        delta=$((100 * (mb - ma) / ma))
        printf '%-12s %10s %10s %7s%%\n' "$name" "$ma" "$mb" "$delta"
    fi
    rm -f "$tmpA" "$tmpB"
done
