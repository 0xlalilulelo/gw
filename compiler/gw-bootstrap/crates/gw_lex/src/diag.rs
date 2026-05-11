//! Structured compiler diagnostics.
//!
//! See `docs/architecture.md` Part D.6. The Phase 0 form is intentionally
//! minimal — primary span + optional secondaries + notes + suggestion;
//! rustc-style multi-line rendering with snippet excerpts arrives in a
//! later phase.

use crate::source::{SourceMap, Span};
use std::fmt;

/// Severity of a diagnostic.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Severity {
    /// Compilation cannot succeed.
    Error,
    /// Compilation continues; programmer should look.
    Warning,
    /// Informational note (typically attached to another diagnostic).
    Note,
    /// Hint or suggested fix.
    Help,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
            Self::Help => "help",
        };
        f.write_str(s)
    }
}

/// Stable error code, e.g. `E0001`.
///
/// Codes are unique per category; the lexer reserves `E0001..E0099`,
/// the parser `E0100..E0199`, and so on. Codes never change once shipped
/// so that documentation links remain valid.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ErrorCode(pub u16);

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E{:04}", self.0)
    }
}

/// A span-attached annotation.
pub struct Label {
    /// Span this label points at.
    pub span: Span,
    /// Optional message; empty string for an unlabelled highlight.
    pub message: String,
}

impl Label {
    /// Construct a label.
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

/// A machine-applicable suggestion.
pub struct Suggestion {
    /// Span to replace.
    pub span: Span,
    /// Replacement text.
    pub replacement: String,
    /// User-facing description of the suggestion.
    pub message: String,
}

/// A single diagnostic.
pub struct Diagnostic {
    /// Severity.
    pub severity: Severity,
    /// Error code (optional — synthetic diagnostics may omit).
    pub code: Option<ErrorCode>,
    /// Top-level message.
    pub message: String,
    /// Primary span — where the problem is.
    pub primary: Label,
    /// Secondary spans with their own messages.
    pub secondary: Vec<Label>,
    /// Free-form notes appended after the spans.
    pub notes: Vec<String>,
    /// Optional machine-applicable fix.
    pub suggestion: Option<Suggestion>,
}

impl Diagnostic {
    /// Build an error-severity diagnostic.
    pub fn error(code: ErrorCode, primary: Label, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: Some(code),
            message: message.into(),
            primary,
            secondary: Vec::new(),
            notes: Vec::new(),
            suggestion: None,
        }
    }

    /// Build a warning-severity diagnostic.
    pub fn warning(code: ErrorCode, primary: Label, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code: Some(code),
            message: message.into(),
            primary,
            secondary: Vec::new(),
            notes: Vec::new(),
            suggestion: None,
        }
    }

    /// Builder: append a free-form note.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Builder: append a secondary label.
    pub fn with_secondary(mut self, label: Label) -> Self {
        self.secondary.push(label);
        self
    }

    /// Builder: attach a machine-applicable suggestion.
    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.suggestion = Some(suggestion);
        self
    }
}

/// Bag of accumulated diagnostics.
///
/// Tracks how many errors have been pushed for cheap "did anything fail?"
/// checks without scanning the vec.
#[derive(Default)]
pub struct DiagBag {
    diags: Vec<Diagnostic>,
    errors: u32,
}

impl DiagBag {
    /// Construct an empty bag.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a diagnostic.
    pub fn push(&mut self, d: Diagnostic) {
        if d.severity == Severity::Error {
            self.errors = self.errors.saturating_add(1);
        }
        self.diags.push(d);
    }

    /// Whether at least one error-severity diagnostic has been pushed.
    pub fn has_errors(&self) -> bool {
        self.errors > 0
    }

    /// Number of error-severity diagnostics.
    pub fn error_count(&self) -> u32 {
        self.errors
    }

    /// Total number of diagnostics (all severities).
    pub fn len(&self) -> usize {
        self.diags.len()
    }

    /// Whether the bag is empty.
    pub fn is_empty(&self) -> bool {
        self.diags.is_empty()
    }

    /// Iterate diagnostics in insertion order.
    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.diags.iter()
    }

    /// Consume the bag and return the underlying vec.
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.diags
    }

    /// Drain all diagnostics from `other` into `self`, preserving
    /// insertion order and accumulating the error count. Used by the
    /// driver in Phase 2 increment F.1 to fold per-file parse
    /// diagnostics into the build's primary bag.
    pub fn merge(&mut self, mut other: DiagBag) {
        for d in other.diags.drain(..) {
            self.push(d);
        }
    }
}

/// Render a diagnostic as a single line for snapshot tests and minimal CLI
/// output. Format:
///
/// ```text
/// E0001 error: unexpected character `?` at <name>:1:5
/// ```
///
/// rustc-style multi-line rendering with snippet excerpts is a Phase 2+
/// concern.
pub fn render_simple(d: &Diagnostic, sm: &SourceMap) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    if let Some(code) = d.code {
        let _ = write!(out, "{code} ");
    }
    let _ = write!(out, "{}: {}", d.severity, d.message);
    if let Some(file) = sm.get(d.primary.span.file) {
        let (line, col) = file.line_col(d.primary.span.start);
        let _ = write!(out, " at {}:{}:{}", file.name, line, col);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::FileId;

    #[test]
    fn error_code_format() {
        assert_eq!(format!("{}", ErrorCode(42)), "E0042");
        assert_eq!(format!("{}", ErrorCode(1)), "E0001");
    }

    #[test]
    fn diagbag_counts() {
        let mut bag = DiagBag::new();
        let span = Span::new(FileId(1), 0, 1);
        bag.push(Diagnostic::error(
            ErrorCode(1),
            Label::new(span, ""),
            "oops",
        ));
        bag.push(Diagnostic::warning(
            ErrorCode(2),
            Label::new(span, ""),
            "watch out",
        ));
        assert_eq!(bag.error_count(), 1);
        assert_eq!(bag.len(), 2);
        assert!(bag.has_errors());
    }
}
