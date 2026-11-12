//! Randomized renderer property, the render-side companion of the parse
//! and checker mirrors: `human` and `to_json` never panic on any
//! diagnostic shape over any registered files — spans in bounds, stale
//! offsets past the end of their file (the R20 clamp), labels crossing
//! files in any order (the R25 per-label `-->` naming), empty spans, and
//! every registered error code. Rendering is data out, never an abort.

use ds_diag::{Diagnostic, ErrorCode, Files, LabeledSpan, Severity, Span, Suggestion, render};

/// xorshift64* PRNG: deterministic, no dependency (mirrors the other
/// fuzz mirrors).
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).expect("bound fits u64").max(1);
        usize::try_from(self.next_u64() % bound).expect("remainder fits usize")
    }
}

/// Three registered files of different lengths and line shapes; stale
/// offsets deliberately overshoot them.
fn fixture_files() -> Files {
    let mut files = Files::new();
    files.add_source("main.ds", "fn main() -> () {\n  println(read(h))\n}\n");
    files.add_source("handle.ds", "const rows = read(h)\nfn take(x: file)\n");
    files.add_source("empty.ds", "");
    files
}

/// A random span for `file`: mostly in bounds (start at a line start,
/// end on the same or next line), sometimes a stale offset past the end,
/// sometimes empty.
fn random_span(rng: &mut Rng, files: &Files, file: (u32, usize)) -> Span {
    let id = ds_diag::FileId::new(file.0);
    let len = u32::try_from(file.1).expect("fits u32");
    match rng.below(6) {
        0 | 1 => {
            let start = u32::try_from(rng.below(file.1 + 1)).expect("fits u32");
            Span::new(id, start, start)
        },
        2 => Span::new(id, len + 1, len + 7),
        3 => Span::new(id, len, len + 3),
        _ => {
            let starts = files.line_starts(id);
            let at = starts[rng.below(starts.len())];
            let reach = u32::try_from(rng.below(40)).expect("fits u32");
            let end = (at + reach).min(len.saturating_sub(1).max(at));
            Span::new(id, at, end.max(at))
        },
    }
}

/// One random diagnostic through both renderers.
fn property(rng: &mut Rng, files: &Files) {
    let code = ErrorCode::ALL[rng.below(ErrorCode::ALL.len())];
    let severity = if rng.below(4) == 0 { Severity::Warning } else { Severity::Error };
    let mut diag = Diagnostic::new(severity, code, format!("msg {}", rng.next_u64() % 97));
    let file_lens: [(u32, usize); 3] = [(0, 40), (1, 46), (2, 0)];
    for label in 0..rng.below(5) {
        let file = file_lens[rng.below(file_lens.len())];
        let span = random_span(rng, files, file);
        let label = if label == rng.below(5) {
            LabeledSpan::primary(span, format!("primary {label}"))
        } else {
            LabeledSpan::secondary(span, String::new())
        };
        diag = diag.with_label(label);
    }
    for _ in 0..rng.below(3) {
        diag = diag.with_note(format!("note {}", rng.next_u64() % 89));
    }
    if rng.below(2) == 0 {
        let file = file_lens[rng.below(file_lens.len())];
        let span = random_span(rng, files, file);
        let suggestion = match rng.below(3) {
            0 => Suggestion::Replace {
                span,
                with: format!("fixed {}", rng.next_u64() % 31),
            },
            1 => Suggestion::Insert {
                span: Span::new(span.file, span.start, span.start),
                text: "inserted".to_owned(),
            },
            _ => Suggestion::Remove { span },
        };
        diag = diag.with_suggestion(suggestion);
    }
    std::hint::black_box(render::human(&diag, files));
    #[cfg(feature = "json")]
    std::hint::black_box(render::to_json(&diag, files));
}

/// Case count; a CI fuzz job can crank it with `FUZZ_RUNS=<n> cargo
/// nextest run`. Invalid or unset values keep the default.
fn fuzz_runs() -> u64 {
    std::env::var("FUZZ_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(240)
}

#[test]
fn random_diagnostics_render_never_panic() {
    let files = fixture_files();
    for seed in 1..=fuzz_runs() {
        let mut rng = Rng(seed | 1);
        property(&mut rng, &files);
    }
}
