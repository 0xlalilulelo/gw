//! GW lexer, source map, span, and diagnostic types.
//!
//! See `docs/architecture.md` Part B.2 (lexer) and Part D.6 (error
//! reporting). The lexer is a hand-written UTF-8 state machine on
//! `&[u8]`; it produces a flat token stream including trivia, plus a
//! [`DiagBag`] of any structured diagnostics encountered.
//!
//! Public entry point: [`lex`].
//!
//! ```ignore
//! use arsenal_lex::{lex, SourceMap};
//!
//! let mut sm = SourceMap::new();
//! let file = sm.add_file("hello.gw", "fn main() {}");
//! let src = sm.get(file).expect("just inserted").contents.as_bytes();
//! let (tokens, diags) = lex(file, src);
//! assert!(!diags.has_errors());
//! ```

pub mod diag;
pub mod keyword;
pub mod lexer;
pub mod source;
pub mod token;

pub use diag::{render_simple, DiagBag, Diagnostic, ErrorCode, Label, Severity, Suggestion};
pub use keyword::KEYWORDS;
pub use lexer::lex;
pub use source::{BytePos, FileId, SourceFile, SourceMap, Span};
pub use token::{Token, TokenKind};
