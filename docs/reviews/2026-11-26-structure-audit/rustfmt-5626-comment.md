# Draft: comment to post on rust-lang/rustfmt#5626

Repro data for this pathology, with a construct that is a bit more common than the examples above: a method call whose last argument is a block closure. `rustfmt 1.12.0-nightly (5f4effd579 2026-11-13)`, linux x86_64, default config, no `rustfmt.toml`.

Depth 12 of `.is_some_and(|rest| { ... })`, each closure body being the next call (27 lines):

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

`time rustfmt --edition 2024 --check repro.rs`, adding or removing one nesting level per row:

| nesting depth | wall time |
| --- | --- |
| 8  | 0.10 s |
| 9  | 0.30 s |
| 10 | 0.84 s |
| 11 | 2.5 s |
| 12 | 7.4 s |
| 13 | 23.1 s |

That is 2.8x-3.1x per level, so depth 20 is unreachable.

Two facts that may help whoever picks this up:

- It is not nesting in general. The same 12-deep shape written as plain nested `g(` calls formats in 33 ms.
- No configuration option avoids it, and `max_width` makes it worse: at depth 10, default is 0.84 s, `max_width=120` is 1.3 s, `max_width=200` is 4.6 s. Wider lines mean more fit/no-fit alternatives per level.

Both stable 1.9.0 and nightly 1.10.0/1.12.0 behave the same, and `--edition 2021`/`2024` behave the same.

Real-world trigger: a unit test asserting a 7-part source composition via `.strip_prefix(..).is_some_and(|rest| ...)` chaining. At 11 levels `--check` had not finished after 19 minutes at 100% CPU and was killed. Users hit this with ordinary `Option::is_some_and` chains, not only with generated code.

Mechanism, matching the analysis in #7077: the last-arg block closure is rewritten once as an expression (`rewrite_closure_expr`) and, on failure, once wrapped in a block (`rewrite_closure_with_block`); because `rewrite_closure_expr`'s `allow_multi_line` does not list `ExprKind::Closure`, the child closure never counts as brace-providing, so each level rewrites its whole subtree twice. `overflow`'s `rewrite_last_closure` and the general `rewrite_closure` entry point both funnel into that pair. #7077 removed the blow-up by treating closures as brace-providing there, which was closed because it changed `|x| |y| {}` formatting. A narrower fix could keep the existing closure-join formatting and only take the shortcut when the body is a nested closure that is already going to be multi-line.

Workaround for affected code: flatten the chain (e.g. compare the joined result of a `Vec` of parts) or `#[rustfmt::skip]`. Reporting it as a depth limit with an error would also be better than unbounded CI time, which is how this surfaced for us.
