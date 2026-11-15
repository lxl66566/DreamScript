//! The `ds.pkg` manifest: model, structured-diagnostics parser, and
//! canonical serializer. A manifest is the lock, so its serialized bytes
//! are a pure function of the model — deps sorted by name, three-component
//! versions, LF line ends, exactly one trailing newline — and every
//! non-canonical spelling (unknown key or section, duplicate key, reordered
//! inline fields, uppercase hex, zero-prefixed components) is drift the
//! parser reports as a DS4014 diagnostic instead of silently accepting.

use std::{collections::BTreeMap, fmt::Write};

use ds_diag::{Diagnostic, ErrorCode, FileId, Span};
use ds_mod::Hash128;

use crate::version::{Version, VersionError};

/// The package name grammar: an optional `@` sigil, then `/`-separated
/// segments of lowercase ASCII letters, digits, `_`, and `-` (`@std`,
/// `acme/pipeline`). Names are lowercase by design: they are ecosystem
/// identifiers, not free text.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    fn segment(segment: &str) -> bool {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    }
    let rest = name.strip_prefix('@').unwrap_or(name);
    let mut segments = rest.split('/');
    segments.next().is_some_and(segment) && segments.all(segment)
}

/// One manifest's model: the package's own identity and its dependency
/// bounds. Constructed by [`Manifest::parse`] or built by hand; a hand-built
/// name outside the grammar above is a caller bug the serializer's debug
/// assertions catch.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Manifest {
    /// The package's name.
    pub name: String,
    /// The package's own exact version.
    pub version: Version,
    /// The dependency bounds, keyed by package name. The map's name order
    /// is the canonical serialization order.
    pub deps: BTreeMap<String, DepBound>,
}

/// One declared dependency: a minimum version and the content hash of that
/// version's package, pinned by the declaring manifest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DepBound {
    /// The minimum accepted version; resolution unifies demands by picking
    /// the maximum declared minimum.
    pub ver: Version,
    /// The blake3-128 content hash of the dep's package at `ver`.
    pub hash: Hash128,
}

impl Manifest {
    /// Parses manifest text into a model, reporting every drift it can
    /// recover from (one diagnostic per line) before refusing. `file` names
    /// the source the spans index; callers rendering the diagnostics
    /// register the text under it.
    ///
    /// # Errors
    ///
    /// A non-empty vector carries every DS4014 diagnostic found; the parse
    /// refuses rather than guessing a model.
    pub fn parse(text: &str, file: FileId) -> Result<Self, Vec<Diagnostic>> {
        let mut parse = Parse {
            state: State::new(file),
        };
        parse.run(text);
        if parse.state.name.is_none() {
            parse
                .state
                .drift_text(0, 0, "the manifest declares no `name`");
        }
        if parse.state.version.is_none() {
            parse
                .state
                .drift_text(0, 0, "the manifest declares no `version`");
        }
        if parse.state.diagnostics.is_empty() {
            let state = parse.state;
            Ok(Manifest {
                name: state.name.unwrap_or_default(),
                version: state.version.unwrap_or_default(),
                deps: state.deps.unwrap_or_default(),
            })
        } else {
            Err(parse.state.diagnostics)
        }
    }

    /// The canonical text: one byte sequence per model, so a resolved
    /// graph of manifests serializes byte-identically across runs.
    #[must_use]
    pub fn to_text(&self) -> String {
        debug_assert!(
            valid_name(&self.name),
            "manifest name must be grammar-valid"
        );
        let mut out = format!("name = \"{}\"\nversion = \"{}\"\n", self.name, self.version);
        if self.deps.is_empty() {
            return out;
        }
        out.push_str("\n[deps]\n");
        for (name, bound) in &self.deps {
            debug_assert!(valid_name(name), "dep name must be grammar-valid");
            // one write per dep line: String formatting never fails
            let _ = writeln!(
                out,
                "{name} = {{ ver = \"{}\", hash = \"blake3:{}\" }}",
                bound.ver, bound.hash
            );
        }
        out
    }
}

/// Why a hash value is not one.
pub(crate) enum HashValueError {
    /// No `scheme:` prefix, or a scheme other than `blake3` (the one scheme
    /// today; the prefix stays in the grammar so the manifest records its
    /// digest vocabulary).
    Scheme,
    /// The hex part is not exactly 32 digits.
    Length,
    /// A character outside lowercase hex.
    Hex,
}

/// Parses the hash value spelling into a digest.
pub(crate) fn parse_hash(text: &str) -> Result<Hash128, HashValueError> {
    let hex = text.strip_prefix("blake3:").ok_or(HashValueError::Scheme)?;
    let bytes = hex.as_bytes();
    if bytes.len() != 32 {
        return Err(HashValueError::Length);
    }
    let mut digest = [0u8; 16];
    for (index, pair) in bytes.chunks(2).enumerate() {
        let hi = hex_digit(pair[0]).ok_or(HashValueError::Hex)?;
        let lo = hex_digit(pair[1]).ok_or(HashValueError::Hex)?;
        digest[index] = (hi << 4) | lo;
    }
    Ok(Hash128::new(digest))
}

/// One lowercase hex nibble, uppercase deliberately refused as drift.
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// The accumulating model, the per-line cursor, and the diagnostics. Every
/// helper records at most one diagnostic and the line loop skips to the
/// next line, so a broken line never poisons its siblings.
struct State {
    /// The line being read.
    line: String,
    /// The line's byte offset within the whole text.
    base: usize,
    /// The cursor within `line`.
    at: usize,
    /// The source's file id, for spans.
    file: FileId,
    /// The package name, once read.
    name: Option<String>,
    /// The package version, once read.
    version: Option<Version>,
    /// The deps table, `None` until `[deps]` opens it.
    deps: Option<BTreeMap<String, DepBound>>,
    /// Every drift found so far.
    diagnostics: Vec<Diagnostic>,
}

impl State {
    fn new(file: FileId) -> Self {
        Self {
            line: String::new(),
            base: 0,
            at: 0,
            file,
            name: None,
            version: None,
            deps: None,
            diagnostics: Vec::new(),
        }
    }

    /// A span over `[start, end)` of the current line, lifted into the
    /// whole text.
    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(
            self.file,
            u32::try_from(self.base + start).unwrap_or(u32::MAX),
            u32::try_from(self.base + end).unwrap_or(u32::MAX),
        )
    }

    /// Records one DS4014 diagnostic over the current line's
    /// `[start, end)`.
    fn drift(&mut self, start: usize, end: usize, message: String) {
        let span = self.span(start, end);
        self.diagnostics.push(
            Diagnostic::error(ErrorCode::PackageManifestMalformed, message)
                .with_primary(span, "here"),
        );
    }

    /// Records one DS4014 diagnostic over a whole-text byte range (the
    /// missing-key diagnostics, which have no line to point at).
    fn drift_text(&mut self, start: usize, end: usize, message: &str) {
        let span = Span::new(
            self.file,
            u32::try_from(start).unwrap_or(u32::MAX),
            u32::try_from(end).unwrap_or(u32::MAX),
        );
        self.diagnostics.push(
            Diagnostic::error(ErrorCode::PackageManifestMalformed, message.to_owned())
                .with_primary(span, "here"),
        );
    }

    /// Skips spaces and tabs; newlines are the entry separator, not space.
    fn spaces(&mut self) {
        while self.line[self.at..].starts_with([' ', '\t']) {
            self.at += 1;
        }
    }

    fn at_end(&self) -> bool {
        self.at >= self.line.len()
    }

    /// Consumes `byte` after optional spaces; a refusal moves nothing (the
    /// caller's diagnostic spans the offending text).
    fn eat(&mut self, byte: u8) -> bool {
        self.spaces();
        if self.line.as_bytes().get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// Reads a run of word bytes: ASCII alphanumerics, `_`, `-`, `@`, `/`.
    fn word(&mut self) -> (usize, usize) {
        self.spaces();
        let start = self.at;
        while self
            .line
            .as_bytes()
            .get(self.at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'@' | b'/'))
        {
            self.at += 1;
        }
        (start, self.at)
    }

    /// The word span's text.
    fn text(&self, span: (usize, usize)) -> &str {
        &self.line[span.0..span.1]
    }

    /// Reads a quoted ASCII-printable string literal; escapes are
    /// deliberately absent (values are constrained vocabularies). Returns
    /// the contents and the literal's whole span.
    fn string(&mut self) -> Result<(&str, (usize, usize)), ()> {
        if !self.eat(b'"') {
            let at = self.at;
            self.drift(at, self.line.len(), "expected a quoted string".to_owned());
            return Err(());
        }
        let start = self.at;
        let Some(len) = self.line[start..].find('"') else {
            self.at = self.line.len();
            self.drift(
                start - 1,
                self.line.len(),
                "string is unterminated".to_owned(),
            );
            return Err(());
        };
        let contents = &self.line[start..start + len];
        if let Some(bad) = contents.bytes().position(|b| !matches!(b, 0x20..=0x7e)) {
            self.drift(
                start + bad,
                start + bad + 1,
                "strings hold printable ASCII only".to_owned(),
            );
            return Err(());
        }
        self.at = start + len + 1;
        Ok((contents, (start - 1, self.at)))
    }

    /// Accepts only nothing-or-comment after an entry, refusing trailing
    /// content. Answers whether the rest of the line was clean.
    fn trailing(&mut self, what: &str) -> bool {
        self.spaces();
        if self.at_end() || self.line[self.at..].starts_with('#') {
            return true;
        }
        let start = self.at;
        self.at = self.line.len();
        self.drift(
            start,
            self.line.len(),
            format!("unexpected content after {what}"),
        );
        false
    }
}

/// The line loop over the whole text, driving one [`State`].
struct Parse {
    state: State,
}

impl Parse {
    /// Walks the text line by line. `\r\n` line ends are tolerated (one
    /// trailing `\r` strips); the canonical writer emits LF only.
    fn run(&mut self, text: &str) {
        let mut offset = 0;
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            self.state.line.clear();
            self.state.line.push_str(line);
            self.state.base = offset;
            self.state.at = 0;
            self.line_entry();
            offset += raw.len() + 1;
        }
    }

    /// Classifies one line: blank or comment, section header, top-level
    /// key, or dep entry.
    fn line_entry(&mut self) {
        let state = &mut self.state;
        state.spaces();
        if state.at_end() || state.line[state.at..].starts_with('#') {
            return;
        }
        if state.line[state.at..].starts_with('[') {
            self.section();
            return;
        }
        let key_span = state.word();
        let key = state.text(key_span).to_owned();
        if key.is_empty() {
            state.drift(key_span.0, state.line.len(), "expected a key".to_owned());
            return;
        }
        if state.deps.is_some() {
            self.dep_entry(&key, key_span);
        } else {
            self.top_entry(&key, key_span);
        }
    }

    /// `[deps]` is the one ruled section this round; anything else is drift
    /// (the `[artifacts]` and `[build]` sections join with the registry
    /// round's model).
    fn section(&mut self) {
        let state = &mut self.state;
        let start = state.at;
        state.at += 1;
        let name_span = state.word();
        let name = state.text(name_span).to_owned();
        if !state.eat(b']') {
            state.drift(
                start,
                state.line.len(),
                "expected `]` closing the section header".to_owned(),
            );
            return;
        }
        let end = state.at;
        if !state.trailing("the section header") {
            return;
        }
        if name != "deps" {
            state.drift(
                start,
                end,
                format!("unknown section `{name}`; the grammar is name, version, and [deps]"),
            );
            return;
        }
        if state.deps.is_some() {
            state.drift(start, end, "duplicate `[deps]` section".to_owned());
            return;
        }
        state.deps = Some(BTreeMap::new());
    }

    /// `name = "..."` or `version = "..."` before any section.
    fn top_entry(&mut self, key: &str, key_span: (usize, usize)) {
        let state = &mut self.state;
        if !state.eat(b'=') {
            state.drift(
                key_span.1,
                state.line.len(),
                format!("expected `=` after `{key}`"),
            );
            return;
        }
        let Ok((value, value_span)) = state.string() else {
            return;
        };
        let value = value.to_owned();
        match key {
            "name" => {
                if !valid_name(&value) {
                    state.drift(
                        value_span.0,
                        value_span.1,
                        format!(
                            "`{value}` is not a package name (lowercase `/`-separated segments, \
                             optional leading `@`)"
                        ),
                    );
                    return;
                }
                if state.name.replace(value).is_some() {
                    state.drift(key_span.0, key_span.1, "duplicate `name` key".to_owned());
                }
            },
            "version" => match Version::parse(&value) {
                Ok(version) => {
                    if state.version.replace(version).is_some() {
                        state.drift(key_span.0, key_span.1, "duplicate `version` key".to_owned());
                    }
                },
                Err(error) => Self::version_drift(state, value_span, &error),
            },
            _ => state.drift(
                key_span.0,
                key_span.1,
                format!("unknown key `{key}`; the grammar is name, version, and [deps]"),
            ),
        }
        state.trailing("the value");
    }

    /// `dep = { ver = "...", hash = "blake3:..." }`, fields in canonical
    /// order only.
    fn dep_entry(&mut self, key: &str, key_span: (usize, usize)) {
        let state = &mut self.state;
        if !valid_name(key) {
            state.drift(
                key_span.0,
                key_span.1,
                format!(
                    "`{key}` is not a package name (lowercase `/`-separated segments, optional \
                     leading `@`)"
                ),
            );
            return;
        }
        if !state.eat(b'=') {
            state.drift(
                key_span.1,
                state.line.len(),
                format!("expected `=` after `{key}`"),
            );
            return;
        }
        if !state.eat(b'{') {
            state.drift(
                state.at,
                state.line.len(),
                "expected `{` opening the dep table".to_owned(),
            );
            return;
        }
        let field = state.word();
        if state.text(field) != "ver" {
            state.drift(
                field.0,
                field.1.max(field.0 + 3),
                "expected `ver` first inside the dep table".to_owned(),
            );
            return;
        }
        if !state.eat(b'=') {
            state.drift(
                state.at,
                state.line.len(),
                "expected `=` after `ver`".to_owned(),
            );
            return;
        }
        let Ok((ver_text, ver_span)) = state.string() else {
            return;
        };
        let ver = match Version::parse(ver_text) {
            Ok(ver) => ver,
            Err(error) => {
                Self::version_drift(state, ver_span, &error);
                return;
            },
        };
        if !state.eat(b',') {
            state.drift(
                state.at,
                state.line.len(),
                "expected `,` between the dep fields".to_owned(),
            );
            return;
        }
        let field = state.word();
        if state.text(field) != "hash" {
            state.drift(
                field.0,
                field.1.max(field.0 + 4),
                "expected `hash` second inside the dep table".to_owned(),
            );
            return;
        }
        if !state.eat(b'=') {
            state.drift(
                state.at,
                state.line.len(),
                "expected `=` after `hash`".to_owned(),
            );
            return;
        }
        let Ok((hash_text, hash_span)) = state.string() else {
            return;
        };
        let Ok(hash) = parse_hash(hash_text) else {
            state.drift(
                hash_span.0,
                hash_span.1,
                "hash must be `blake3:` followed by 32 lowercase hex digits".to_owned(),
            );
            return;
        };
        if !state.eat(b'}') {
            state.drift(
                state.at,
                state.line.len(),
                "expected `}` closing the dep table".to_owned(),
            );
            return;
        }
        state.trailing("the dep entry");
        let deps = state
            .deps
            .as_mut()
            .expect("dep entries only parse inside [deps]");
        if deps
            .insert(key.to_owned(), DepBound { ver, hash })
            .is_some()
        {
            state.drift(key_span.0, key_span.1, format!("duplicate dep `{key}`"));
        }
    }

    /// Lifts a version syntax error into a diagnostic over the offending
    /// component inside the literal.
    fn version_drift(state: &mut State, literal: (usize, usize), error: &VersionError) {
        let at = match error {
            VersionError::Empty | VersionError::TooManyComponents => literal.0 + 1,
            VersionError::BadComponent { at } => literal.0 + 1 + at,
        };
        state.drift(
            at,
            (at + 1).min(literal.1),
            format!("malformed version: {error}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Manifest, Vec<Diagnostic>> {
        Manifest::parse(text, FileId::new(0))
    }

    #[test]
    fn canonical_text_round_trips_through_the_model() {
        let text = concat!(
            "name = \"acme/pipeline\"\n",
            "version = \"0.4.0\"\n",
            "\n",
            "[deps]\n",
            "@std = { ver = \"1.2.0\", hash = \"blake3:0f0e0d0c0b0a09080706050403020100\" }\n",
            "csv = { ver = \"0.7.0\", hash = \"blake3:100f0e0d0c0b0a090807060504030201\" }\n",
        );
        let manifest = parse(text).expect("canonical text parses");
        assert_eq!(manifest.to_text(), text);
        // and the model is stable across a second pass
        let again = parse(&manifest.to_text()).expect("round two parses");
        assert_eq!(again, manifest);
    }

    #[test]
    fn empty_deps_omit_the_section_and_round_trip() {
        let manifest = parse("name = \"app\"\nversion = \"1.0.0\"\n").expect("parses");
        assert!(manifest.deps.is_empty());
        assert_eq!(manifest.to_text(), "name = \"app\"\nversion = \"1.0.0\"\n");
    }

    #[test]
    fn versions_normalize_to_three_components() {
        let manifest = parse(concat!(
            "name = \"app\"\n",
            "version = \"2\"\n",
            "\n",
            "[deps]\n",
            "@std = { ver = \"1.2\", hash = \"blake3:00000000000000000000000000000000\" }\n",
        ))
        .expect("short versions parse");
        assert_eq!(manifest.version, Version::new(2, 0, 0));
        assert_eq!(manifest.deps["@std"].ver, Version::new(1, 2, 0));
        assert!(manifest.to_text().contains("version = \"2.0.0\""));
        assert!(manifest.to_text().contains("ver = \"1.2.0\""));
    }

    #[test]
    fn crlf_line_ends_and_comments_parse() {
        let text = concat!(
            "# the lock\r\n",
            "name = \"app\" # the package\r\n",
            "version = \"0.1.0\"\r\n",
            "\r\n",
            "[deps]\r\n",
            "# one dep\r\n",
            "@std = { ver = \"1.0.0\", hash = \"blake3:0102030405060708090a0b0c0d0e0f10\" }\r\n",
        );
        let manifest = parse(text).expect("tolerated line ends parse");
        assert_eq!(manifest.name, "app");
        assert_eq!(manifest.deps.len(), 1);
    }

    #[test]
    fn drift_reports_code_and_span() {
        let text = "name = \"app\"\nversionn = \"1.0.0\"\n";
        let diags = parse(text).expect_err("unknown key refuses");
        // the unknown-key diagnostic plus the missing `version` it leaves
        assert_eq!(diags.len(), 2);
        assert!(diags.iter().all(|diag| diag.code.as_str() == "DS4014"));
        // the second line's key starts at byte 13 and runs 8 bytes
        assert_eq!(diags[0].labels[0].span.start, 13);
        assert_eq!(diags[0].labels[0].span.end, 21);
        assert_eq!(diags[1].message, "the manifest declares no `version`");
    }

    #[test]
    fn every_drift_class_refuses() {
        let cases: &[&str] = &[
            "version = \"1.0.0\"\n",
            "name = \"app\"\n",
            "name = \"app\"\nname = \"app\"\nversion = \"1.0.0\"\n",
            "name = \"App\"\nversion = \"1.0.0\"\n",
            "name = \"app\"\nversion = \"01.0.0\"\n",
            "name = \"app\"\nversion = \"1.0.0\"\n[deps]\n[deps]\n",
            "name = \"app\"\nversion = \"1.0.0\"\n[artifacts]\n",
            "name = \"app\"\nversion = \"1.0.0\"\n[deps]\nx = { hash = \
             \"blake3:00000000000000000000000000000000\", ver = \"1.0.0\" }\n",
            "name = \"app\"\nversion = \"1.0.0\"\n[deps]\nx = { ver = \"1.0.0\", hash = \
             \"blake3:ABCDEF00000000000000000000000000\" }\n",
            "name = \"app\"\nversion = \"1.0.0\"\n[deps]\nx = { ver = \"1.0.0\", hash = \
             \"blake3:0000\" }\n",
            "name = \"app\"\nversion = \"1.0.0\"\n[deps]\nx = { ver = \"1.0.0\", hash = \
             \"sha256:00000000000000000000000000000000\" }\n",
            "name = \"app\"\nversion = \"1.0.0\"\n[deps]\nx = { ver = \"1.0.0\", hash = \
             \"blake3:00000000000000000000000000000000\" }\nx = { ver = \"1.0.0\", hash = \
             \"blake3:00000000000000000000000000000000\" }\n",
            "name = \"app\"\nversion = \"1.0.0\"\n[deps]\nx { ver = \"1.0.0\", hash = \
             \"blake3:00000000000000000000000000000000\" }\n",
            "name = \"app\"\nversion = \"1.0.0\"\nname = \"app\" trailing\n",
        ];
        for &text in cases {
            let Err(diags) = parse(text) else {
                panic!("{text:?} must refuse")
            };
            assert!(
                !diags.is_empty()
                    && diags
                        .iter()
                        .all(|diag| diag.code == ErrorCode::PackageManifestMalformed),
                "{text:?}: at least one DS4014 diagnostic"
            );
        }
    }

    #[test]
    fn missing_keys_report_at_the_manifest_head() {
        let diags = parse("version = \"1.0.0\"\n").expect_err("missing name refuses");
        assert_eq!(
            diags[diags.len() - 1].message,
            "the manifest declares no `name`"
        );
        let diags = parse("name = \"app\"\n").expect_err("missing version refuses");
        assert_eq!(
            diags[diags.len() - 1].message,
            "the manifest declares no `version`"
        );
    }

    #[test]
    fn reference_manifest_shape_is_valid() {
        let text = concat!(
            "name = \"pipeline\"\n",
            "version = \"0.4.0\"\n",
            "\n",
            "[deps]\n",
            "std = { ver = \"1.2\", hash = \"blake3:9f2c0000000000000000000000000000\" }\n",
            "csv = { ver = \"0.7\", hash = \"blake3:1a880000000000000000000000000000\" }\n",
        );
        let manifest = parse(text).expect("the corpus reference parses");
        assert_eq!(manifest.name, "pipeline");
        assert_eq!(manifest.version, Version::new(0, 4, 0));
        assert_eq!(manifest.deps["std"].ver, Version::new(1, 2, 0));
        assert_eq!(manifest.deps.len(), 2);
    }
}
