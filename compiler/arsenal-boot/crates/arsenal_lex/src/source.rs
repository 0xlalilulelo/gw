//! Source files, byte-positions, spans, and the [`SourceMap`] that ties
//! them together.
//!
//! See `docs/architecture.md` Part B.2 (lexer spans) and Part D.6
//! (diagnostics carry spans into a `SourceMap`).

use std::fmt;

/// Identifies a source file within a [`SourceMap`].
///
/// IDs are assigned in the order [`SourceMap::add_file`] is called and never
/// reused. [`FileId::NONE`] is reserved for synthetic spans (compiler-
/// generated nodes with no source) and is **not** a valid lookup key.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct FileId(pub(crate) u32);

impl FileId {
    /// Sentinel "no file" id, used by [`Span::synthetic`].
    pub const NONE: FileId = FileId(0);

    /// Raw u32 representation. Useful for diagnostic printing and
    /// deterministic ordering.
    pub const fn to_raw(self) -> u32 {
        self.0
    }
}

/// 0-based byte offset into a source file.
pub type BytePos = u32;

/// Half-open byte range `[start, end)` within a single file.
///
/// Spans are the universal source-location currency: they flow from the
/// lexer through every later pass and ultimately into diagnostics.
/// Empty spans (`start == end`) are valid and used for "insertion-point"
/// diagnostics (e.g. "expected `;` here").
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Span {
    /// File this span lives in.
    pub file: FileId,
    /// Inclusive byte offset of the first byte of the span.
    pub start: BytePos,
    /// Exclusive byte offset one past the last byte of the span.
    pub end: BytePos,
}

impl Span {
    /// Construct a span without checking `start <= end`. The caller is
    /// responsible for the invariant.
    pub const fn new(file: FileId, start: BytePos, end: BytePos) -> Self {
        Self { file, start, end }
    }

    /// Synthetic span with no associated file.
    pub const fn synthetic() -> Self {
        Self {
            file: FileId::NONE,
            start: 0,
            end: 0,
        }
    }

    /// Length of the span in bytes.
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    /// Whether the span covers zero bytes.
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Smallest span covering both `self` and `other`.
    ///
    /// Both spans must live in the same file; debug-asserts otherwise.
    pub fn merge(self, other: Span) -> Span {
        debug_assert_eq!(self.file, other.file, "cannot merge spans across files");
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}..{}", self.file.0, self.start, self.end)
    }
}

/// One loaded source file.
///
/// Owns its display name and contents and pre-computes line starts for
/// cheap line/column lookup. Contents are guaranteed valid UTF-8 because
/// they are stored as `String`.
pub struct SourceFile {
    /// Display name (filesystem path, `<stdin>`, `<repl>`, etc.).
    pub name: String,
    /// File contents.
    pub contents: String,
    /// Byte offset of the start of each line. `line_starts[0]` is always 0;
    /// subsequent entries are positions immediately following each `\n`.
    line_starts: Vec<BytePos>,
}

impl SourceFile {
    fn new(name: String, contents: String) -> Self {
        let mut line_starts = vec![0u32];
        for (i, &b) in contents.as_bytes().iter().enumerate() {
            if b == b'\n' {
                let next = i.saturating_add(1);
                if next <= u32::MAX as usize {
                    line_starts.push(next as u32);
                }
            }
        }
        Self {
            name,
            contents,
            line_starts,
        }
    }

    /// 1-indexed `(line, column)` of the given byte offset.
    ///
    /// Column is counted in **bytes**, not code points. Out-of-range
    /// `pos` is clamped to end-of-file.
    pub fn line_col(&self, pos: BytePos) -> (u32, u32) {
        let pos = pos.min(self.contents.len() as u32);
        let line_idx = match self.line_starts.binary_search(&pos) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };
        // line_starts is non-empty by construction (always contains 0), so
        // line_idx is always in range.
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or(0);
        let line = (line_idx as u32).saturating_add(1);
        let col = pos.saturating_sub(line_start).saturating_add(1);
        (line, col)
    }
}

/// In-memory map of all source files known to the compiler. The lexer,
/// parser, and downstream passes reference files by [`FileId`] and look
/// up source through this map.
#[derive(Default)]
pub struct SourceMap {
    /// Indexed by `(FileId.0 - 1)` because `FileId(0)` is the sentinel.
    files: Vec<SourceFile>,
}

impl SourceMap {
    /// Construct an empty `SourceMap`.
    pub fn new() -> Self {
        Self { files: Vec::new() }
    }

    /// Insert a file. Returns the newly assigned [`FileId`].
    pub fn add_file(&mut self, name: impl Into<String>, contents: impl Into<String>) -> FileId {
        let file = SourceFile::new(name.into(), contents.into());
        self.files.push(file);
        FileId(self.files.len() as u32)
    }

    /// Look up a file. Returns `None` for [`FileId::NONE`] or any id not
    /// known to this map.
    pub fn get(&self, id: FileId) -> Option<&SourceFile> {
        if id.0 == 0 {
            return None;
        }
        self.files.get((id.0 as usize).wrapping_sub(1))
    }

    /// 1-indexed `(line, col)` of `span.start` in the named file.
    /// Returns `None` if the span's file is not in this map.
    pub fn line_col(&self, span: Span) -> Option<(u32, u32)> {
        self.get(span.file).map(|f| f.line_col(span.start))
    }

    /// Borrow the source slice covered by `span`. Returns `None` if the
    /// span is out of range or the file is unknown.
    pub fn slice(&self, span: Span) -> Option<&str> {
        let f = self.get(span.file)?;
        let s = span.start as usize;
        let e = span.end as usize;
        f.contents.get(s..e)
    }

    /// Number of files in the map.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the map contains no files.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_basic() {
        let mut sm = SourceMap::new();
        let f = sm.add_file("t", "ab\ncd\nef");
        let file = sm.get(f).expect("file present");
        assert_eq!(file.line_col(0), (1, 1));
        assert_eq!(file.line_col(1), (1, 2));
        assert_eq!(file.line_col(2), (1, 3));
        assert_eq!(file.line_col(3), (2, 1));
        assert_eq!(file.line_col(6), (3, 1));
        // out-of-range clamps to EOF
        assert_eq!(file.line_col(999), (3, 3));
    }

    #[test]
    fn span_merge() {
        let f = FileId(7);
        let a = Span::new(f, 5, 10);
        let b = Span::new(f, 8, 15);
        assert_eq!(a.merge(b), Span::new(f, 5, 15));
    }

    #[test]
    fn slice_and_get() {
        let mut sm = SourceMap::new();
        let f = sm.add_file("t", "hello world");
        let s = Span::new(f, 6, 11);
        assert_eq!(sm.slice(s), Some("world"));
        assert!(sm.get(FileId::NONE).is_none());
    }

    #[test]
    fn out_of_range_slice_returns_none() {
        let mut sm = SourceMap::new();
        let f = sm.add_file("t", "abc");
        assert!(sm.slice(Span::new(f, 0, 10)).is_none());
    }
}
