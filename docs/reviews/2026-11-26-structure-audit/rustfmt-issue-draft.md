# Draft: standalone issue for rust-lang/rustfmt (post only if a dedicated report is wanted; expect "duplicate of #5626")

Title (78 chars): rustfmt takes exponential time on nested block closures passed to method calls

---

## Summary

`rustfmt`'s rewrite time is exponential in the nesting depth of a method call whose last argument is a block closure. Each extra nesting level costs roughly 3x, so a 25-line file that `rustc` compiles in milliseconds takes tens of seconds to format, and one more level makes it effectively hang.

The same nesting depth written as plain nested function calls formats in tens of milliseconds, so this is the closure-argument code path, not nesting or `max_width` pressure in general.

## Reproduction

`repro.rs` (nesting depth 12):

```rust
fn f() {
    A.is_some_and(|rest| {
        rest.is_some_and(|rest| {
            rest.is_some_and(|rest| {
                rest.is_some_and(|rest| {
                    rest.is_some_and(|rest| {
                        rest.is_some_and(|rest| {
                            rest.is_some_and(|rest| {
                                rest.is_some_and(|rest| {
                                    rest.is_some_and(|rest| {
                                        rest.is_some_and(|rest| {
                                            rest.is_some_and(|rest| {
                                                rest.is_some_and(|rest| {
                                                    x
                                                })
                                            })
                                        })
                                    })
                                })
                            })
                        })
                    })
                })
            })
        })
    });
}
```

The names are unresolved by design; rustfmt only parses, so this needs no `rustc` run. Change the number of `is_some_and` layers to get a different depth.

```bash
$ time rustfmt --edition 2024 --check repro.rs
```

There is no `rustfmt.toml`; the command above uses the default configuration.

## Timings

Measured with `rustfmt 1.12.0-nightly (5f4effd579 2026-11-13)` on linux x86_64, default config, `--check`, wall clock, one run each (repeat runs agree within 2%):

| closure nesting depth | file size | wall time |
| --- | --- | --- |
| 8  | 19 lines | 0.10 s |
| 9  | 21 lines | 0.30 s |
| 10 | 23 lines | 0.84 s |
| 11 | 25 lines | 2.5 s |
| 12 | 27 lines | 7.4 s |
| 13 | 29 lines | 23.1 s |

The factor per level is 2.8x-3.1x. Depth 8 is partly dominated by process startup, so the asymptotic factor is clearest from depth 10 upward.

## Control: same nesting depth, plain function calls

```rust
fn f() {
    g(
        g(
            g(
                g(
                    g(
                        g(
                            g(
                                g(
                                    g(
                                        g(
                                            g(
                                                g(
                                                    x
                                                )
                                            )
                                        )
                                    )
                                )
                            )
                        )
                    )
                )
            )
        )
    );
}
```

`time rustfmt --edition 2024 --check` on this depth-12 file: 33 ms. So it is not nesting depth per se; it is the block-closure argument path.

## No configuration option avoids it

At nesting depth 10 (default: 0.84 s), with `--unstable-features`:

| config | wall time |
| --- | --- |
| default | 0.84 s |
| `max_width=120` | 1.3 s |
| `max_width=200` | 4.6 s |
| `wrap_comments=true,comment_width=80` | 0.99 s |
| `normalize_comments=true` | 0.94 s |
| `format_code_in_doc_comments=true` | 0.86 s |

No option short-circuits the path; widening `max_width` makes it worse, which is consistent with more line-fit alternatives being explored per level.

## Versions

Reproduced on:

- `rustfmt 1.9.0-stable` (current stable channel)
- `rustfmt 1.10.0-nightly`
- `rustfmt 1.12.0-nightly (5f4effd579 2026-11-13)`

`--edition 2024` and `--edition 2021` behave identically.

## Expected behavior

Formatting time should be roughly linear (or at worst polynomial) in the size of the input, as it is for the plain-call control. If that is not achievable for deeply nested closures, a documented depth limit and a clear error ("input too deeply nested to format") is preferable to unbounded CPU time, because `cargo fmt --check` is commonly run in CI.

## Real-world trigger

A unit test that asserts a 7-part source composition, written as a chain of `.strip_prefix(..).is_some_and(|rest| ...)` calls (each closure body is the next call in the chain). At 11 nesting levels `rustfmt --check` had not finished after 19 minutes at 100% CPU on this machine and was killed. Nothing exotic is involved: `Option::is_some_and` with a block closure, chained.

```rust
assert!(src
    .strip_prefix("a:").is_some_and(|rest|
    rest.strip_prefix("b:").is_some_and(|rest|
    rest.strip_prefix("c:").is_some_and(|rest|
    rest.strip_prefix("d:").is_some_and(|rest|
    rest.strip_prefix("e:").is_some_and(|rest|
    rest.strip_prefix("f:").is_some_and(|rest|
    rest.strip_prefix("g:").is_some_and(|rest| rest == expected))))))));
```

## Workaround

Flatten the assertion, e.g. split into a `Vec<&str>` of parts and compare joined output, or annotate the function/statement with `#[rustfmt::skip]`. Both keep CI green; neither is discoverable from an error message, since nothing reports the blow-up.

## Environment

- OS: linux x86_64
- `rustc`/`rustfmt` from rustup, nightly `5f4effd579 2026-11-13` (reproduced on stable 1.9.0 and nightly 1.10.0 as well)
- default configuration, no `rustfmt.toml`, `--edition 2024`

## Additional context (already-known related reports)

- #5626 (`rustfmt hangs`, open, `A-closures`): generic tracking report for deeply nested closures; this issue is the same root cause, with a minimal method-call reproducer and a measured scaling curve.
- #6536 (`Deep nests hang rustfmt`, closed as duplicate of #5626): chained `.and_then(|c| ...)`/`or_else(|| ...)` closures, i.e. the same construct as here.
- #6420 (`Formatting code that heavily uses closures appears to take an extremely long time.`, closed as duplicate of #5626).
- #7077 (closed unmerged): diagnosis and a candidate fix. Treating closures as brace-providing in `rewrite_closure_expr`'s `allow_multi_line` removes the `O(2^N)`, but changes `|x| |y| {}` formatting, so it was closed pending a style decision.
- #5139 (merged, then reverted in June 2022): `format_expr` memoization fixed this class of blow-up, but was reverted for output-stability reasons, so current releases have no memoization on these paths.
