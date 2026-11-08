//! Source registry and byte-offset span model.

use core::ops::Range;

/// Identifier of a source file, handed out by [`Files::add_source`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FileId(u32);

impl FileId {
    /// Wraps a raw identifier.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns the raw identifier.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// A half-open byte range `[start, end)` within one source file.
///
/// Offsets are raw byte offsets into the registered source. Invariant:
/// `start <= end`, enforced by [`Span::new`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Span {
    /// The file the range lies in.
    pub file: FileId,
    /// First byte offset, inclusive.
    pub start: u32,
    /// Last byte offset, exclusive.
    pub end: u32,
}

impl Span {
    /// Creates a span spanning `[start, end)` in `file`.
    ///
    /// # Panics
    ///
    /// Panics if `start > end`.
    #[must_use]
    pub const fn new(file: FileId, start: u32, end: u32) -> Self {
        assert!(start <= end, "span start must not exceed end");
        Self { file, start, end }
    }

    /// Whether the span covers no bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// A 1-based line and column position within a source file.
///
/// Columns count Unicode scalar values from the start of the line, so a
/// column points past characters, not bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Location {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column, counted in Unicode scalar values.
    pub col: u32,
}

/// One registered source file; internal bookkeeping of [`Files`].
#[derive(Clone, Debug)]
struct SourceFile {
    name: String,
    source: String,
    len: u32,
    /// Byte offset at which each 1-based line starts; entry 0 is always 0.
    line_starts: Vec<u32>,
}

/// Registry of named source files, supporting byte-offset to line/column
/// lookup through a precomputed line index.
///
/// Lines are terminated by `\n`; a `\r` directly before it is treated as
/// part of the terminator (CRLF). A trailing newline opens one final empty
/// line, and an empty file has exactly one empty line.
#[derive(Clone, Debug, Default)]
pub struct Files {
    files: Vec<SourceFile>,
}

impl Files {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `source` under `name` and returns its file id.
    ///
    /// # Panics
    ///
    /// Panics if the source is longer than `u32` byte offsets (4 GiB).
    pub fn add_source(&mut self, name: impl Into<String>, source: &str) -> FileId {
        let file = FileId::new(u32::try_from(self.files.len()).expect("file count overflows u32"));
        let len = u32::try_from(source.len()).expect("source overflows u32 byte offsets");
        let mut line_starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(u32::try_from(i + 1).expect("source overflows u32 byte offsets"));
            }
        }
        self.files.push(SourceFile {
            name: name.into(),
            source: source.to_owned(),
            len,
            line_starts,
        });
        file
    }

    fn get(&self, file: FileId) -> &SourceFile {
        self.files.get(file.as_u32() as usize).unwrap_or_else(|| {
            panic!("unknown FileId {}", file.as_u32());
        })
    }

    /// The name `source` was registered under.
    #[must_use]
    pub fn name(&self, file: FileId) -> &str {
        &self.get(file).name
    }

    /// The full source text of `file`.
    #[must_use]
    pub fn source(&self, file: FileId) -> &str {
        &self.get(file).source
    }

    /// Byte offset at which each 1-based line starts; entry 0 is always 0.
    #[must_use]
    pub fn line_starts(&self, file: FileId) -> &[u32] {
        &self.get(file).line_starts
    }

    /// Number of lines; every file has at least one.
    #[must_use]
    pub fn line_count(&self, file: FileId) -> u32 {
        u32::try_from(self.line_starts(file).len()).expect("line count overflows u32")
    }

    /// The 1-based line and column of `byte_offset`.
    ///
    /// # Panics
    ///
    /// Panics if `byte_offset` exceeds the source length.
    #[must_use]
    pub fn location(&self, file: FileId, byte_offset: u32) -> Location {
        let f = self.get(file);
        assert!(
            byte_offset <= f.len,
            "byte offset {} out of bounds for `{}` (len {})",
            byte_offset,
            f.name,
            f.len
        );
        // Last line start <= byte_offset; starts[0] == 0 always satisfies it.
        let line_idx = f.line_starts.partition_point(|&s| s <= byte_offset) - 1;
        let col = char_distance(&f.source, f.line_starts[line_idx], byte_offset);
        Location {
            line: u32::try_from(line_idx).expect("line index overflows u32") + 1,
            col: u32::try_from(col + 1).expect("column overflows u32"),
        }
    }

    /// Byte range of the 1-based `line`, excluding its terminator (`\n` or
    /// `\r\n`).
    ///
    /// # Panics
    ///
    /// Panics if there is no such line.
    #[must_use]
    pub fn line_span(&self, file: FileId, line: u32) -> Range<u32> {
        let f = self.get(file);
        let starts = &f.line_starts;
        assert!(
            line >= 1 && line as usize <= starts.len(),
            "no line {line} in `{}`",
            f.name
        );
        let start = starts[(line - 1) as usize];
        let Some(&next) = starts.get(line as usize) else {
            // Last line: no terminator follows, so nothing to strip.
            return start..f.len;
        };
        let end = next - 1;
        // Strip the \r of a CRLF terminator.
        let end = if end > start && f.source.as_bytes()[(end - 1) as usize] == b'\r' {
            end - 1
        } else {
            end
        };
        start..end
    }
}

/// Number of Unicode scalar values in `source[from..to]`, falling back to
/// byte distance when the offsets do not land on char boundaries.
pub(crate) fn char_distance(source: &str, from: u32, to: u32) -> usize {
    debug_assert!(from <= to);
    match source.get((from as usize)..(to as usize)) {
        Some(slice) => slice.chars().count(),
        None => (to - from) as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_has_one_empty_line() {
        let mut files = Files::new();
        let f = files.add_source("empty.ds", "");
        assert_eq!(files.location(f, 0), Location { line: 1, col: 1 });
        assert_eq!(files.line_span(f, 1), 0..0);
        assert_eq!(files.line_count(f), 1);
    }

    #[test]
    fn offsets_at_eof() {
        let mut files = Files::new();
        let f = files.add_source("a.ds", "abc");
        assert_eq!(files.location(f, 0), Location { line: 1, col: 1 });
        assert_eq!(files.location(f, 3), Location { line: 1, col: 4 });
        let f = files.add_source("b.ds", "a\nb");
        assert_eq!(files.location(f, 1), Location { line: 1, col: 2 });
        assert_eq!(files.location(f, 3), Location { line: 2, col: 2 });
        // A trailing newline opens one final empty line.
        let f = files.add_source("c.ds", "a\n");
        assert_eq!(files.line_starts(f), &[0, 2]);
        assert_eq!(files.location(f, 2), Location { line: 2, col: 1 });
        assert_eq!(files.line_span(f, 2), 2..2);
    }

    #[test]
    fn crlf_terminators_are_excluded_from_line_text() {
        let mut files = Files::new();
        let f = files.add_source("a.ds", "a\r\nbb\r\n");
        assert_eq!(files.line_starts(f), &[0, 3, 7]);
        assert_eq!(files.line_span(f, 1), 0..1);
        assert_eq!(files.line_span(f, 2), 3..5);
        assert_eq!(files.location(f, 5), Location { line: 2, col: 3 });
        // A lone \r is ordinary content, not a line break.
        let f = files.add_source("b.ds", "x\r");
        assert_eq!(files.line_count(f), 1);
        assert_eq!(files.line_span(f, 1), 0..2);
    }

    #[test]
    fn columns_count_chars_not_bytes() {
        let mut files = Files::new();
        let f = files.add_source("a.ds", "\u{65e5}\u{672c}\u{8a9e}\nab");
        assert_eq!(files.location(f, 3), Location { line: 1, col: 2 });
        // The newline byte itself still belongs to line 1.
        assert_eq!(files.location(f, 9), Location { line: 1, col: 4 });
        assert_eq!(files.location(f, 10), Location { line: 2, col: 1 });
        assert_eq!(files.location(f, 12), Location { line: 2, col: 3 });
    }

    #[test]
    #[should_panic(expected = "span start must not exceed end")]
    fn span_rejects_start_after_end() {
        let _ = Span::new(FileId::new(0), 5, 3);
    }

    #[test]
    fn file_ids_are_distinct() {
        let mut files = Files::new();
        let a = files.add_source("a.ds", "");
        let b = files.add_source("b.ds", "");
        assert_ne!(a, b);
        assert_eq!(files.name(a), "a.ds");
        assert_eq!(files.name(b), "b.ds");
    }
}
