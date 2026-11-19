#!/bin/sh
# Cross-language kernel runner. Modes:
#   run.sh check [kernel...]   run every kernel on every available interpreter,
#                              compare the final `result:` line against the
#                              kernel's .golden; exit nonzero on any mismatch.
#   run.sh bench [kernel...]   wall-clock each run once and print a table.
#                              Numbers are meaningful only under an exclusive
#                              flock with the machine otherwise idle (see
#                              README.md); they are never committed.
# The ds interpreter is the release CLI: build it first with
#   CARGO_TARGET_DIR=<shared target> cargo build -p ds-cli --release
# and point DS_BIN at the binary when it is not at <repo>/target/release/ds.
# Every ds invocation forces the cold path (`--no-cache`): the suite
# measured the full pipeline before the script-mode dsmod cache existed,
# the numbers stay comparable, and no run inherits or writes a user
# cache directory.

set -u

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
kdir="$here/kernels"

# ---- interpreters ------------------------------------------------------------

# The release ds binary: DS_BIN wins, else the repo's shared target dir
# (release/ds.exe on Windows, release/ds elsewhere).
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
# One python slot: python3 first (the usual spelling on Linux), then python.
python_bin=""
for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1; then
        python_bin=$candidate
        break
    fi
done

lua_bin=""
for candidate in lua lua5.4 lua5.3; do
    if command -v "$candidate" >/dev/null 2>&1; then
        lua_bin=$candidate
        break
    fi
done

node_bin=""
command -v node >/dev/null 2>&1 && node_bin=node

# Runs one interpreter on one kernel file; kernel stdout flows through.
run_kernel() {
    interp=$1
    file=$2
    case $interp in
    ds) "$ds_bin" run --no-cache "$file" ;;
    python) "$python_bin" "$file" ;;
    lua) "$lua_bin" "$file" ;;
    node) "$node_bin" "$file" ;;
    esac
}

kernel_file() {
    case $2 in
    ds) printf '%s/%s.ds' "$kdir" "$1" ;;
    python) printf '%s/%s.py' "$kdir" "$1" ;;
    lua) printf '%s/%s.lua' "$kdir" "$1" ;;
    node) printf '%s/%s.mjs' "$kdir" "$1" ;;
    esac
}

# ---- arguments ---------------------------------------------------------------

mode=${1:-}
[ $# -gt 0 ] && shift
selected=${*:-}

all_kernels=$(ls "$kdir"/*.ds 2>/dev/null | sed 's|.*/||; s|\.ds$||' | sort)
if [ -z "$all_kernels" ]; then
    echo "run.sh: no kernels under $kdir" >&2
    exit 1
fi

if [ "$mode" != check ] && [ "$mode" != bench ]; then
    echo "usage: run.sh check|bench [kernel...]" >&2
    echo "  kernels:$all_kernels" | tr '\n' ' ' >&2
    echo >&2
    exit 2
fi

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

if [ -z "$kernels" ]; then
    echo "run.sh: no kernel matches:$selected" >&2
    exit 2
fi

# The ds interpreter is the suite's subject, not a peer: a missing binary
# fails the run (the peer languages still SKIP — only ds is required). A
# silent SKIP here would leave every ds kernel unmeasured with the run
# still green.
if [ -z "$ds_bin" ]; then
    echo "run.sh: no ds binary (set DS_BIN or build: CARGO_TARGET_DIR=<target> cargo build -p ds-cli --release)" >&2
    exit 1
fi

# ---- check -------------------------------------------------------------------

if [ "$mode" = check ]; then
    failures=0
    for name in $kernels; do
        golden=$(sed -n 's/\r$//;1p' "$kdir/$name.golden" 2>/dev/null)
        if [ -z "$golden" ]; then
            echo "FAIL $name: missing $name.golden"
            failures=$((failures + 1))
            continue
        fi
        for interp in ds python lua node; do
            eval "bin=\$${interp}_bin"
            if [ -z "$bin" ]; then
                echo "SKIP $name $interp: interpreter not found"
                continue
            fi
            file=$(kernel_file "$name" "$interp")
            # The kernel's own exit status survives the filter pipeline
            # only through a temp file: `$?` after a pipe is the last
            # stage's, so a crashing interpreter with a `result:` line on
            # stdout would read green through sed | grep | tail.
            log=$(mktemp)
            run_kernel "$interp" "$file" >"$log" 2>&1
            status=$?
            out=$(sed 's/\r$//' "$log" | grep '^result:' | tail -n 1)
            rm -f "$log"
            if [ "$status" -ne 0 ]; then
                echo "FAIL $name $interp: interpreter exited $status"
                failures=$((failures + 1))
            elif [ -z "$out" ]; then
                echo "FAIL $name $interp: no \`result:\` line on stdout (did the kernel run?)"
                failures=$((failures + 1))
            elif [ "$out" != "$golden" ]; then
                echo "FAIL $name $interp: expected [$golden] got [$out]"
                failures=$((failures + 1))
            else
                echo "OK   $name $interp: $out"
            fi
        done
    done
    if [ "$failures" -gt 0 ]; then
        echo "check: $failures failure(s)"
        exit 1
    fi
    echo "check: all green"
    exit 0
fi

# ---- bench -------------------------------------------------------------------
# Wall-clock one run per kernel and interpreter (stdout discarded). Repeat the
# invocation under an exclusive flock for stable numbers.

printf '%-12s %-8s %10s\n' KERNEL INTERP MS
for name in $kernels; do
    for interp in ds python lua node; do
        eval "bin=\$${interp}_bin"
        if [ -z "$bin" ]; then
            printf '%-12s %-8s %10s\n' "$name" "$interp" SKIP
            continue
        fi
        file=$(kernel_file "$name" "$interp")
        t0=$(date +%s%N)
        run_kernel "$interp" "$file" >/dev/null 2>&1
        status=$?
        t1=$(date +%s%N)
        if [ "$status" -ne 0 ]; then
            printf '%-12s %-8s %10s\n' "$name" "$interp" "FAIL($status)"
        else
            printf '%-12s %-8s %10d\n' "$name" "$interp" $(((t1 - t0) / 1000000))
        fi
    done
done
