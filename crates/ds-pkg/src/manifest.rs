//! The `ds.pkg` manifest: model, structured-diagnostics parser, and
//! canonical serializer. A manifest is the lock, so its serialized bytes
//! are a pure function of the model — deps and artifacts sorted by name,
//! three-component versions, LF line ends, exactly one trailing newline —
//! and every non-canonical spelling (unknown key or section, duplicate
//! key, reordered inline fields, uppercase hex, zero-prefixed components)
//! is drift the parser reports as a DS4014 diagnostic instead of silently
//! accepting.

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

/// One manifest's model: the package's own identity, its dependency
/// bounds, and its native artifacts. Constructed by [`Manifest::parse`]
/// or built by hand; a hand-built name outside the grammar above is a
/// caller bug the serializer's debug assertions catch.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Manifest {
    /// The package's name.
    pub name: String,
    /// The package's own exact version.
    pub version: Version,
    /// The dependency bounds, keyed by package name. The map's name order
    /// is the canonical serialization order.
    pub deps: BTreeMap<String, DepBound>,
    /// The native artifacts, keyed by artifact name: prebuilt,
    /// per-target, content-hashed libraries consumed through the C-ABI
    /// seam. The map's name order is the canonical serialization order.
    pub artifacts: BTreeMap<String, ArtifactBound>,
}

/// One declared dependency: a registry pin (a minimum version and the
/// content hash of that version's package) or a local directory shape (a
/// relative path naming the dep's package root, resolution reading the
/// package from source and keying the store by the content it actually
/// saw). One shape per name: a manifest declaring both for one dep is
/// drift, and a graph demanding one name at both shapes refuses at
/// resolution.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DepBound {
    /// `name = { ver = "...", hash = "blake3:..." }` inline under
    /// `[deps]`.
    Pin {
        /// The minimum accepted version; resolution unifies demands by
        /// picking the maximum declared minimum.
        ver: Version,
        /// The blake3-128 content hash of the dep's package at `ver`.
        hash: Hash128,
    },
    /// `[deps.<name>]` with `path = "<relative directory>"`: the dep
    /// serves from the source directory, the store a cache layer.
    Path {
        /// The dep's package root, relative to the declaring package's
        /// root (`..` segments allowed — a sibling directory is the
        /// workspace spelling).
        path: String,
    },
}

/// The dep path grammar: a relative directory, `/`-separated printable
/// ASCII with no backslash, no leading or trailing separator, and no
/// empty or `.` segment (`..` allowed — a sibling directory is the
/// canonical workspace spelling). The path resolves against the
/// declaring package's root.
#[must_use]
pub fn valid_dep_path(path: &str) -> bool {
    !path.is_empty()
        && path.bytes().all(|b| matches!(b, 0x20..=0x7e) && b != b'\\')
        && !path.starts_with('/')
        && !path.ends_with('/')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != ".")
}

/// One declared native artifact: a prebuilt library for one target
/// triple, pinned by the content hash of its bytes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ArtifactBound {
    /// The artifact's exact version.
    pub ver: Version,
    /// The target triple the library was built for.
    pub target: String,
    /// The blake3-128 content hash of the library's bytes.
    pub hash: Hash128,
}

/// The target triple grammar: two to four dash-separated segments of
/// lowercase ASCII letters, digits, and `_` (`x86_64-pc-windows-msvc`,
/// `aarch64-unknown-linux-gnu`). Triples are ecosystem identifiers, not
/// free text.
#[must_use]
pub fn valid_target(target: &str) -> bool {
    fn segment(segment: &str) -> bool {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }
    let count = target.split('-').count();
    (2..=4).contains(&count) && target.split('-').all(segment)
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
        let mut state = parse.state;
        let paths = std::mem::take(&mut state.paths);
        for (name, entry) in paths {
            let (start, end) = (entry.span.0, entry.span.1);
            let Some(path) = entry.path else {
                state.drift_text(
                    start,
                    end,
                    &format!("the [deps.{name}] subsection declares no `path`"),
                );
                continue;
            };
            if state.deps.contains_key(&name) {
                state.drift_text(
                    start,
                    end,
                    &format!(
                        "the dep `{name}` declares both a registry pin and a local path; one \
                         shape per dep"
                    ),
                );
                continue;
            }
            state.deps.insert(name, DepBound::Path { path });
        }
        if state.diagnostics.is_empty() {
            Ok(Manifest {
                name: state.name.unwrap_or_default(),
                version: state.version.unwrap_or_default(),
                deps: state.deps,
                artifacts: state.artifacts,
            })
        } else {
            Err(state.diagnostics)
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
        let mut paths = Vec::new();
        let mut pinned = false;
        for (name, bound) in &self.deps {
            debug_assert!(valid_name(name), "dep name must be grammar-valid");
            match bound {
                DepBound::Pin { ver, hash } => {
                    if !pinned {
                        pinned = true;
                        out.push_str("\n[deps]\n");
                    }
                    // one write per dep line: String formatting never fails
                    let _ = writeln!(
                        out,
                        "{name} = {{ ver = \"{ver}\", hash = \"blake3:{hash}\" }}"
                    );
                },
                DepBound::Path { path } => {
                    debug_assert!(valid_dep_path(path), "dep path must be grammar-valid");
                    paths.push((name, path));
                },
            }
        }
        // the path subsections follow the inline table, names ascending
        for (name, path) in paths {
            let _ = writeln!(out, "\n[deps.{name}]\npath = \"{path}\"");
        }
        if !self.artifacts.is_empty() {
            out.push_str("\n[artifacts]\n");
            for (name, bound) in &self.artifacts {
                debug_assert!(valid_name(name), "artifact name must be grammar-valid");
                debug_assert!(valid_target(&bound.target), "target must be grammar-valid");
                let _ = writeln!(
                    out,
                    "{name} = {{ ver = \"{}\", target = \"{}\", hash = \"blake3:{}\" }}",
                    bound.ver, bound.target, bound.hash
                );
            }
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

/// The section an entry belongs to: nothing opened yet, `[deps]`, a
/// `[deps.<name>]` subsection, or `[artifacts]` — the ruled sections, in
/// that canonical order (the subsections sorted by name inside the deps
/// zone, after the inline table).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
enum Section {
    /// Before any section header; top-level keys.
    #[default]
    Head,
    /// Inside `[deps]`.
    Deps,
    /// Inside `[deps.<name>]`; the payload is the subsection's dep name.
    DepsSub(String),
    /// Inside `[artifacts]`.
    Artifacts,
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
    /// The section the next entry belongs to.
    section: Section,
    /// The deps table (the inline registry pins).
    deps: BTreeMap<String, DepBound>,
    /// The path subsections: each dep name over its `path` value (none
    /// read yet) and the header's whole-text span.
    paths: BTreeMap<String, PathEntry>,
    /// The artifacts table.
    artifacts: BTreeMap<String, ArtifactBound>,
    /// Every drift found so far.
    diagnostics: Vec<Diagnostic>,
}

/// One `[deps.<name>]` subsection under construction.
#[derive(Clone, Debug, Default)]
struct PathEntry {
    /// The `path` value, once read.
    path: Option<String>,
    /// The subsection header's span over the whole text.
    span: (usize, usize),
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
            section: Section::Head,
            deps: BTreeMap::new(),
            paths: BTreeMap::new(),
            artifacts: BTreeMap::new(),
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
        // The gate borrows inline and the returned slice is bound last: a
        // borrow held across `drift`'s `&mut self` is rejected by NLL.
        if let Some(bad) = self.line[start..start + len]
            .bytes()
            .position(|b| !matches!(b, 0x20..=0x7e))
        {
            self.drift(
                start + bad,
                start + bad + 1,
                "strings hold printable ASCII only".to_owned(),
            );
            return Err(());
        }
        self.at = start + len + 1;
        let contents = &self.line[start..start + len];
        Ok((contents, (start - 1, self.at)))
    }

    /// One `key = "value"` field inside an inline table: the canonical
    /// field name at `place`, then `=`, then a quoted literal. Returns
    /// the literal's contents and whole span.
    fn table_field(&mut self, key: &str, place: &str) -> Result<(String, (usize, usize)), ()> {
        let field = self.word();
        if self.text(field) != key {
            self.drift(
                field.0,
                field.1.max(field.0 + key.len()),
                format!("expected `{key}` {place}"),
            );
            return Err(());
        }
        if !self.eat(b'=') {
            self.drift(
                self.at,
                self.line.len(),
                format!("expected `=` after `{key}`"),
            );
            return Err(());
        }
        self.string().map(|(text, span)| (text.to_owned(), span))
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
    /// key, dep entry, or artifact entry.
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
        match state.section.clone() {
            Section::Head => self.top_entry(&key, key_span),
            Section::Deps => self.dep_entry(&key, key_span),
            Section::DepsSub(_) => self.path_entry(&key, key_span),
            Section::Artifacts => self.artifact_entry(&key, key_span),
        }
    }

    /// `[deps]`, `[deps.<name>]`, and `[artifacts]` are the ruled
    /// sections, in that order (the subsections sorted by name after the
    /// inline table); `[build]` is deferred (source builds wait on the
    /// pinned-toolchain reproducibility story, never permanently
    /// rejected), and anything else is drift.
    fn section(&mut self) {
        let state = &mut self.state;
        let start = state.at;
        state.at += 1;
        let name_span = state.word();
        let name = state.text(name_span).to_owned();
        let dotted = state.line[state.at..].starts_with('.');
        let sub = if dotted {
            state.at += 1;
            let sub_span = state.word();
            let sub = state.text(sub_span).to_owned();
            if sub.is_empty() {
                state.drift(
                    start,
                    state.line.len(),
                    "expected a dep name after `deps.`".to_owned(),
                );
                return;
            }
            Some(sub)
        } else {
            None
        };
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
        match (name.as_str(), sub) {
            ("deps", None) => {
                if state.section == Section::Deps {
                    state.drift(start, end, "duplicate `[deps]` section".to_owned());
                } else if matches!(state.section, Section::Artifacts | Section::DepsSub(_)) {
                    state.drift(
                        start,
                        end,
                        "the `[deps]` table must precede `[artifacts]` and its own subsections"
                            .to_owned(),
                    );
                } else {
                    state.section = Section::Deps;
                }
            },
            ("deps", Some(sub)) => {
                if !valid_name(&sub) {
                    state.drift(
                        start,
                        end,
                        format!(
                            "`{sub}` is not a package name (lowercase `/`-separated segments, \
                             optional leading `@`)"
                        ),
                    );
                    return;
                }
                if matches!(state.section, Section::Artifacts) {
                    state.drift(
                        start,
                        end,
                        "the `[deps.<name>]` subsections must precede `[artifacts]`".to_owned(),
                    );
                    return;
                }
                if let Section::DepsSub(last) = &state.section
                    && sub.as_str() <= last.as_str()
                {
                    state.drift(
                        start,
                        end,
                        format!(
                            "the `[deps.<name>]` subsections sort by name (`{sub}` after `{last}`)"
                        ),
                    );
                    return;
                }
                state.paths.insert(sub.clone(), PathEntry {
                    path: None,
                    span: (state.base + start, state.base + end),
                });
                state.section = Section::DepsSub(sub);
            },
            ("artifacts", None) => {
                if state.section == Section::Artifacts {
                    state.drift(start, end, "duplicate `[artifacts]` section".to_owned());
                } else {
                    state.section = Section::Artifacts;
                }
            },
            ("artifacts", Some(_)) => state.drift(
                start,
                end,
                "the `[artifacts]` grammar has no subsections".to_owned(),
            ),
            ("build", None) => state.drift(
                start,
                end,
                "the `[build]` section is deferred: source builds wait on the pinned-toolchain \
                 reproducibility story; prebuilt libraries declare under `[artifacts]`"
                    .to_owned(),
            ),
            _ => state.drift(
                start,
                end,
                format!(
                    "unknown section `{name}`; the grammar is name, version, [deps], and \
                     [artifacts]"
                ),
            ),
        }
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
                format!(
                    "unknown key `{key}`; the grammar is name, version, [deps], and [artifacts]"
                ),
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
        let Ok((ver_text, ver_span)) = state.table_field("ver", "first inside the dep table")
        else {
            return;
        };
        let ver = match Version::parse(&ver_text) {
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
        let Ok((hash_text, hash_span)) = state.table_field("hash", "second inside the dep table")
        else {
            return;
        };
        let Ok(hash) = parse_hash(&hash_text) else {
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
        if state
            .deps
            .insert(key.to_owned(), DepBound::Pin { ver, hash })
            .is_some()
        {
            state.drift(key_span.0, key_span.1, format!("duplicate dep `{key}`"));
        }
    }

    /// `path = "<relative directory>"` inside a `[deps.<name>]`
    /// subsection: the only key the shape admits, the value a relative
    /// directory resolving against the declaring package's root.
    fn path_entry(&mut self, key: &str, key_span: (usize, usize)) {
        let state = &mut self.state;
        let Section::DepsSub(sub) = state.section.clone() else {
            return;
        };
        if key != "path" {
            state.drift(
                key_span.0,
                key_span.1,
                format!(
                    "unknown key `{key}`; the `[deps.<name>]` grammar is `path` alone — a \
                     registry pin declares inline under `[deps]`"
                ),
            );
            return;
        }
        if !state.eat(b'=') {
            state.drift(
                key_span.1,
                state.line.len(),
                "expected `=` after `path`".to_owned(),
            );
            return;
        }
        let Ok((value, value_span)) = state.string().map(|(text, span)| (text.to_owned(), span))
        else {
            return;
        };
        if !valid_dep_path(&value) {
            state.drift(
                value_span.0,
                value_span.1,
                format!(
                    "`{value}` is not a relative directory path (`/`-separated segments, `..` \
                     allowed, no leading or trailing separator)"
                ),
            );
            return;
        }
        state.trailing("the path value");
        let entry = state.paths.entry(sub).or_default();
        if entry.path.replace(value.clone()).is_some() {
            state.drift(key_span.0, key_span.1, "duplicate `path` key".to_owned());
        }
    }

    /// `name = { ver = "...", target = "...", hash = "blake3:..." }`,
    /// fields in canonical order only: one prebuilt library per target
    /// triple, pinned by the content hash of its bytes.
    fn artifact_entry(&mut self, key: &str, key_span: (usize, usize)) {
        let state = &mut self.state;
        if !valid_name(key) {
            state.drift(
                key_span.0,
                key_span.1,
                format!(
                    "`{key}` is not an artifact name (lowercase `/`-separated segments, optional \
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
                "expected `{` opening the artifact table".to_owned(),
            );
            return;
        }
        let Ok((ver_text, ver_span)) = state.table_field("ver", "first inside the artifact table")
        else {
            return;
        };
        let ver = match Version::parse(&ver_text) {
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
                "expected `,` between the artifact fields".to_owned(),
            );
            return;
        }
        let Ok((target_text, target_span)) =
            state.table_field("target", "second inside the artifact table")
        else {
            return;
        };
        if !valid_target(&target_text) {
            state.drift(
                target_span.0,
                target_span.1,
                format!(
                    "`{target_text}` is not a target triple (two to four dash-separated lowercase \
                     segments)"
                ),
            );
            return;
        }
        if !state.eat(b',') {
            state.drift(
                state.at,
                state.line.len(),
                "expected `,` between the artifact fields".to_owned(),
            );
            return;
        }
        let Ok((hash_text, hash_span)) =
            state.table_field("hash", "third inside the artifact table")
        else {
            return;
        };
        let Ok(hash) = parse_hash(&hash_text) else {
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
                "expected `}` closing the artifact table".to_owned(),
            );
            return;
        }
        state.trailing("the artifact entry");
        if state
            .artifacts
            .insert(key.to_owned(), ArtifactBound {
                ver,
                target: target_text,
                hash,
            })
            .is_some()
        {
            state.drift(
                key_span.0,
                key_span.1,
                format!("duplicate artifact `{key}`"),
            );
        }
    }

    /// Lifts a version syntax error into a diagnostic over the offending
    /// component or identifier inside the literal.
    fn version_drift(state: &mut State, literal: (usize, usize), error: &VersionError) {
        let at = match error {
            VersionError::Empty | VersionError::TooManyComponents | VersionError::TooLong => {
                literal.0 + 1
            },
            VersionError::BadComponent { at }
            | VersionError::BadPre { at }
            | VersionError::BadBuild { at } => literal.0 + 1 + at,
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
        assert!(manifest.artifacts.is_empty());
        assert_eq!(manifest.to_text(), "name = \"app\"\nversion = \"1.0.0\"\n");
    }

    #[test]
    fn artifacts_round_trip_after_deps() {
        let text = concat!(
            "name = \"app\"\n",
            "version = \"1.0.0\"\n",
            "\n",
            "[deps]\n",
            "@std = { ver = \"1.2.0\", hash = \"blake3:0f0e0d0c0b0a09080706050403020100\" }\n",
            "\n",
            "[artifacts]\n",
            "mymath = { ver = \"0.4.1\", target = \"x86_64-pc-windows-msvc\", hash = \
             \"blake3:100f0e0d0c0b0a090807060504030201\" }\n",
            "zlib = { ver = \"1.3.0\", target = \"aarch64-unknown-linux-gnu\", hash = \
             \"blake3:00000000000000000000000000000000\" }\n",
        );
        let manifest = parse(text).expect("artifacts parse");
        assert_eq!(manifest.artifacts["mymath"].ver, Version::new(0, 4, 1));
        assert_eq!(
            manifest.artifacts["mymath"].target,
            "x86_64-pc-windows-msvc"
        );
        assert_eq!(manifest.to_text(), text);
        let again = parse(&manifest.to_text()).expect("round two parses");
        assert_eq!(again, manifest);
        // and a short version normalizes to three components on the way out
        let short = parse(concat!(
            "name = \"app\"\nversion = \"1.0.0\"\n\n[artifacts]\n",
            "zlib = { ver = \"1.3\", target = \"a-b\", hash = \
             \"blake3:00000000000000000000000000000000\" }\n",
        ))
        .expect("short versions parse");
        assert_eq!(short.artifacts["zlib"].ver, Version::new(1, 3, 0));
        assert!(short.to_text().contains("ver = \"1.3.0\""));
    }

    #[test]
    fn artifact_drift_refuses_with_its_own_words() {
        // a bad target triple points at the literal
        let diags = parse(concat!(
            "name = \"app\"\n",
            "version = \"1.0.0\"\n",
            "\n",
            "[artifacts]\n",
            "mymath = { ver = \"1.0.0\", target = \"Windows\", hash = \
             \"blake3:00000000000000000000000000000000\" }\n",
        ))
        .expect_err("an invalid triple refuses");
        assert!(
            diags
                .iter()
                .any(|diag| diag.message.contains("not a target triple")),
            "the drift names the grammar: {:?}",
            diags[0].message
        );
        // the [build] deferral names its own story, never a bare unknown section
        let diags = parse("name = \"app\"\nversion = \"1.0.0\"\n[build]\n")
            .expect_err("[build] stays deferred");
        assert!(diags[0].message.contains("`[build]` section is deferred"));
        assert!(diags[0].message.contains("[artifacts]"));
        // field order and duplicate names are drift
        let cases: &[&str] = &[
            concat!(
                "name = \"app\"\nversion = \"1.0.0\"\n[artifacts]\n",
                "m = { target = \"a-b\", ver = \"1.0.0\", hash = \
                 \"blake3:00000000000000000000000000000000\" }\n",
            ),
            concat!(
                "name = \"app\"\nversion = \"1.0.0\"\n[artifacts]\n",
                "m = { ver = \"1.0.0\", hash = \"blake3:00000000000000000000000000000000\", \
                 target = \"a-b\" }\n",
            ),
            concat!(
                "name = \"app\"\nversion = \"1.0.0\"\n[artifacts]\n",
                "m = { ver = \"1.0.0\", target = \"a-b\", hash = \
                 \"blake3:00000000000000000000000000000000\" }\n",
                "m = { ver = \"1.0.0\", target = \"a-b\", hash = \
                 \"blake3:00000000000000000000000000000000\" }\n",
            ),
            concat!(
                "name = \"app\"\nversion = \"1.0.0\"\n[artifacts]\n",
                "m = { ver = \"1.0.0\", target = \"a\", hash = \
                 \"blake3:00000000000000000000000000000000\" }\n",
            ),
            concat!(
                "name = \"app\"\nversion = \"1.0.0\"\n[artifacts]\n",
                "m = { ver = \"1.0.0\", target = \"a-b-c-d-e\", hash = \
                 \"blake3:00000000000000000000000000000000\" }\n",
            ),
            "name = \"app\"\nversion = \"1.0.0\"\n[artifacts]\n[artifacts]\n",
            concat!(
                "name = \"app\"\nversion = \"1.0.0\"\n[artifacts]\n[deps]\n",
                "m = { ver = \"1.0.0\", hash = \"blake3:00000000000000000000000000000000\" }\n",
            ),
        ];
        for &text in cases {
            let Err(diags) = parse(text) else {
                panic!("{text:?} must refuse")
            };
            assert!(!diags.is_empty(), "{text:?} reports its drift");
        }
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
        assert_eq!(manifest.deps["@std"], DepBound::Pin {
            ver: Version::new(1, 2, 0),
            hash: Hash128::ZERO
        });
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
            "name = \"app\"\nversion = \"1.0.0\"\n[build]\n",
            "name = \"app\"\nversion = \"1.0.0\"\n[other]\n",
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
    fn pre_release_and_build_metadata_round_trip() {
        let text = concat!(
            "name = \"app\"\n",
            "version = \"1.2.3-rc.1+build.42\"\n",
            "\n",
            "[deps]\n",
            "@std = { ver = \"1.0.0-alpha\", hash = \"blake3:00000000000000000000000000000000\" \
             }\n",
        );
        let manifest = parse(text).expect("pre-release versions parse");
        assert_eq!(manifest.version.to_string(), "1.2.3-rc.1+build.42");
        assert!(
            matches!(
                &manifest.deps["@std"],
                DepBound::Pin { ver, .. } if ver.to_string() == "1.0.0-alpha"
            ),
            "the pre-release pin parses"
        );
        assert_eq!(
            manifest.to_text(),
            text,
            "the segments round-trip canonically"
        );
    }

    #[test]
    fn malformed_segments_drift_at_their_offset() {
        let text = "name = \"app\"\nversion = \"1.0.0-rc.01\"\n";
        let diags = parse(text).expect_err("the leading-zero pre-release identifier refuses");
        // the offending identifier `01` sits three bytes into `rc.01`
        let at = text.find("rc.01").expect("the fixture names its offset") + 3;
        assert_eq!(
            diags[0].labels[0].span.start,
            u32::try_from(at).expect("the offset fits")
        );
        assert!(
            diags[0]
                .message
                .contains("malformed pre-release identifier")
        );
        let diags = parse("name = \"app\"\nversion = \"1.0.0+bui ld\"\n")
            .expect_err("the malformed build identifier refuses");
        assert!(
            diags[0]
                .message
                .contains("malformed build-metadata identifier")
        );
    }

    #[test]
    fn path_deps_round_trip_in_canonical_order() {
        // the inline table first, the subsections after it, names ascending
        let text = concat!(
            "name = \"app\"\n",
            "version = \"0.1.0\"\n",
            "\n",
            "[deps]\n",
            "@std = { ver = \"1.2.0\", hash = \"blake3:0f0e0d0c0b0a09080706050403020100\" }\n",
            "\n",
            "[deps.@lib]\n",
            "path = \"../libs/lib\"\n",
            "\n",
            "[deps.util]\n",
            "path = \"vendor/util\"\n",
        );
        let manifest = parse(text).expect("the path shapes parse");
        assert!(
            matches!(
                &manifest.deps["@lib"],
                DepBound::Path { path } if path == "../libs/lib"
            ),
            "the sibling spelling parses"
        );
        assert!(
            matches!(
                &manifest.deps["util"],
                DepBound::Path { path } if path == "vendor/util"
            ),
            "the in-root spelling parses"
        );
        assert_eq!(manifest.deps.len(), 3);
        assert_eq!(manifest.to_text(), text);
        let again = parse(&manifest.to_text()).expect("round two parses");
        assert_eq!(again, manifest);
        // a path-only manifest omits the inline table
        let path_only = concat!(
            "name = \"app\"\n",
            "version = \"0.1.0\"\n",
            "\n",
            "[deps.util]\n",
            "path = \"../util\"\n",
        );
        let manifest = parse(path_only).expect("the path-only manifest parses");
        assert_eq!(manifest.deps.len(), 1);
        assert_eq!(manifest.to_text(), path_only);
    }

    #[test]
    fn path_dep_drift_refuses_with_its_own_words() {
        // one name at both shapes refuses at the merge
        let both = concat!(
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n",
            "util = { ver = \"1.0.0\", hash = \"blake3:00000000000000000000000000000000\" }\n",
            "\n[deps.util]\npath = \"../util\"\n",
        );
        let diags = parse(both).expect_err("both shapes for one name refuse");
        assert!(
            diags.iter().any(|diag| diag
                .message
                .contains("both a registry pin and a local path")),
            "the merge names its refusal: {:?}",
            diags[0].message
        );
        // the subsection grammar is path alone; an inline pin inside it drifts
        let diags = parse(concat!(
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.util]\n",
            "ver = \"1.0.0\"\n"
        ))
        .expect_err("the wrong key refuses");
        assert!(
            diags[0]
                .message
                .contains("the `[deps.<name>]` grammar is `path` alone"),
            "{:?}",
            diags[0].message
        );
        // the path value grammar: absolute, backslash, dot segment, trailing
        // slash, and a missing path key
        for text in [
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.u]\npath = \"/abs\"\n",
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.u]\npath = \"a\\\\b\"\n",
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.u]\npath = \"./x\"\n",
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.u]\npath = \"x/\"\n",
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.u]\n",
        ] {
            let Err(diags) = parse(text) else {
                panic!("{text:?} must refuse")
            };
            assert!(!diags.is_empty(), "{text:?} reports its drift");
        }
        let diags = parse("name = \"app\"\nversion = \"0.1.0\"\n\n[deps.u]\n")
            .expect_err("the keyless subsection refuses");
        assert!(
            diags
                .iter()
                .any(|diag| diag.message.contains("declares no `path`")),
            "{:?}",
            diags[0].message
        );
    }

    #[test]
    fn path_dep_sections_keep_canonical_order() {
        // the inline table precedes its subsections; the subsections sort by
        // name; [artifacts] follows the deps zone
        let cases: &[&str] = &[
            // [deps] after a subsection
            concat!(
                "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.u]\npath = \"../u\"\n\n[deps]\n",
                "x = { ver = \"1.0.0\", hash = \"blake3:00000000000000000000000000000000\" }\n",
            ),
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.b]\npath = \"../b\"\n\n[deps.a]\npath = \
             \"../a\"\n",
            // duplicate subsection
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.a]\npath = \"../a\"\n\n[deps.a]\npath = \
             \"../a\"\n",
            // a subsection after [artifacts]
            concat!(
                "name = \"app\"\nversion = \"0.1.0\"\n\n[artifacts]\n",
                "m = { ver = \"1.0.0\", target = \"a-b\", hash = \
                 \"blake3:00000000000000000000000000000000\" }\n\n[deps.a]\npath = \"../a\"\n",
            ),
            // artifacts carries no subsections
            "name = \"app\"\nversion = \"0.1.0\"\n\n[artifacts.m]\npath = \"../m\"\n",
            // an empty subsection name
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.]\npath = \"../a\"\n",
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
        assert!(
            matches!(
                &manifest.deps["std"],
                DepBound::Pin { ver, .. } if *ver == Version::new(1, 2, 0)
            ),
            "the short-version pin normalizes"
        );
        assert_eq!(manifest.deps.len(), 2);
    }
}
