//! Parser cursor and error-reporting helpers.
//!
//! See `docs/architecture.md` Part B.3 (parser architecture).
//!
//! The cursor consumes a flat [`Token`] stream produced by the lexer
//! (including trivia) and dispatches significant tokens to grammar rules.
//! Trivia (whitespace, comments) is emitted into the currently-open CST
//! node automatically, in source order — this is how the CST stays
//! lossless without each grammar rule having to think about whitespace.

use arsenal_ast::{CstBuilder, FileArena};
use arsenal_lex::{BytePos, DiagBag, Diagnostic, ErrorCode, FileId, Label, Span, Token, TokenKind};

/// Parser error codes. Reserved range: `E0100..E0199`.
pub mod ec {
    use arsenal_lex::ErrorCode;
    /// Generic unexpected token / expected something else.
    pub const UNEXPECTED_TOKEN: ErrorCode = ErrorCode(100);
    /// Expected a specific token kind that was missing.
    pub const EXPECTED_TOKEN: ErrorCode = ErrorCode(101);
    /// Expected an item at a position where one is required.
    pub const EXPECTED_ITEM: ErrorCode = ErrorCode(102);
    /// Expected a statement.
    pub const EXPECTED_STMT: ErrorCode = ErrorCode(103);
    /// Expected an expression.
    pub const EXPECTED_EXPR: ErrorCode = ErrorCode(104);
    /// Expected a type.
    pub const EXPECTED_TYPE: ErrorCode = ErrorCode(105);
    /// Expected a pattern.
    pub const EXPECTED_PATTERN: ErrorCode = ErrorCode(106);
}

/// Parser state carried through the recursive-descent grammar.
///
/// The lifetime parameters:
/// - `'src` ties the parser to the borrowed token slice and source bytes.
/// - `'arena` ties the parser to the [`FileArena`] handle.
/// - `'bump` ties the parser to the underlying bump arena allocations.
pub struct Parser<'src, 'arena, 'bump> {
    /// Source bytes — held so grammar rules can compare token text
    /// (e.g. distinguish `_` from a real identifier in pattern position).
    pub src: &'src [u8],
    /// Tokens including trivia. `Eof` is the last entry by lexer
    /// invariant.
    pub tokens: &'src [Token],
    /// Current index into `tokens`. May point at trivia; helpers below
    /// look past trivia for "current significant token" queries.
    pub pos: usize,
    /// CST builder. Mutated in place as grammar rules push tokens and
    /// open/close nodes.
    pub builder: CstBuilder<'arena, 'bump>,
    /// File id, copied from the arena for diagnostic reporting.
    pub file: FileId,
    /// Diagnostics produced by both the lexer (forwarded in) and the
    /// parser.
    pub diags: DiagBag,
}

impl<'src, 'arena, 'bump> Parser<'src, 'arena, 'bump> {
    /// Construct a parser. The `diags` bag may already contain lexer
    /// diagnostics; the parser appends to it.
    pub fn new(
        src: &'src [u8],
        tokens: &'src [Token],
        arena: &'arena FileArena<'bump>,
        diags: DiagBag,
    ) -> Self {
        Self {
            src,
            tokens,
            pos: 0,
            builder: CstBuilder::new(arena),
            file: arena.file,
            diags,
        }
    }

    /// Source bytes covered by `span`. Returns `b""` for spans whose
    /// range is outside `src`.
    pub fn span_bytes(&self, span: Span) -> &'src [u8] {
        let s = span.start as usize;
        let e = span.end as usize;
        self.src.get(s..e).unwrap_or(b"")
    }

    // ───── trivia handling ─────────────────────────────────────────────

    /// Index of the next non-trivia token at or after `pos`.
    fn next_significant(&self) -> usize {
        let mut i = self.pos;
        while i < self.tokens.len() && self.tokens[i].kind.is_trivia() {
            i += 1;
        }
        i
    }

    /// Drain leading trivia at `pos` into the currently-open CST node.
    /// Caller must have an open node.
    pub fn skip_trivia_into_node(&mut self) {
        while self.pos < self.tokens.len() && self.tokens[self.pos].kind.is_trivia() {
            let t = self.tokens[self.pos];
            self.builder.push_token(t.kind.into(), t.span);
            self.pos += 1;
        }
    }

    // ───── peek / cursor query ─────────────────────────────────────────

    /// Kind of the current significant token. Returns
    /// [`TokenKind::Eof`] past end-of-stream (the lexer invariant
    /// guarantees an `Eof` sentinel).
    pub fn current(&self) -> TokenKind {
        let i = self.next_significant();
        self.tokens.get(i).map(|t| t.kind).unwrap_or(TokenKind::Eof)
    }

    /// Span of the current significant token (or an empty span at EOF).
    pub fn current_span(&self) -> Span {
        let i = self.next_significant();
        self.tokens
            .get(i)
            .map(|t| t.span)
            .unwrap_or(Span::new(self.file, 0, 0))
    }

    /// Kind of the Nth significant token after the current one
    /// (0 = current). Returns `Eof` past end.
    pub fn peek_at(&self, offset: usize) -> TokenKind {
        let mut count = 0usize;
        let mut i = self.pos;
        while i < self.tokens.len() {
            if !self.tokens[i].kind.is_trivia() {
                if count == offset {
                    return self.tokens[i].kind;
                }
                count += 1;
            }
            i += 1;
        }
        TokenKind::Eof
    }

    /// `true` iff [`Self::current`] equals `kind`.
    pub fn at(&self, kind: TokenKind) -> bool {
        self.current() == kind
    }

    /// `true` iff [`Self::current`] is one of the given kinds.
    pub fn at_any(&self, kinds: &[TokenKind]) -> bool {
        kinds.contains(&self.current())
    }

    /// Byte position of the current significant token's span start (or
    /// EOF position if past end).
    pub fn cur_byte_start(&self) -> BytePos {
        self.current_span().start
    }

    // ───── advance ─────────────────────────────────────────────────────

    /// Drain trivia, then emit the current significant token into the
    /// open CST node and advance past it. Caller must have verified a
    /// significant token is present (don't call at `Eof` unless that's
    /// what you intend to consume).
    pub fn bump_any(&mut self) {
        self.skip_trivia_into_node();
        if self.pos < self.tokens.len() {
            let t = self.tokens[self.pos];
            self.builder.push_token(t.kind.into(), t.span);
            self.pos += 1;
        }
    }

    /// If the current significant token equals `kind`, bump past it
    /// (emitting trivia first) and return `true`. Otherwise, leave the
    /// cursor untouched.
    pub fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.bump_any();
            true
        } else {
            false
        }
    }

    /// Like [`Self::eat`], but emit a diagnostic if `kind` is not
    /// present. Returns whether the token was consumed.
    pub fn expect(&mut self, kind: TokenKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        let span = self.current_span();
        let want = kind.as_str().unwrap_or_else(|| token_kind_label(kind));
        let got = self.current();
        let got_text = got.as_str().unwrap_or_else(|| token_kind_label(got));
        let msg = format!("expected `{want}`, found `{got_text}`");
        self.error(ec::EXPECTED_TOKEN, span, msg);
        false
    }

    // ───── errors ──────────────────────────────────────────────────────

    /// Push an error-severity diagnostic.
    pub fn error(&mut self, code: ErrorCode, span: Span, msg: impl Into<String>) {
        self.diags
            .push(Diagnostic::error(code, Label::new(span, ""), msg));
    }

    /// Generic "unexpected token" diagnostic at the current position.
    pub fn unexpected(&mut self, expected: &str) {
        let span = self.current_span();
        let got = self.current();
        let got_text = got.as_str().unwrap_or(token_kind_label(got));
        self.error(
            ec::UNEXPECTED_TOKEN,
            span,
            format!("unexpected `{got_text}`; expected {expected}"),
        );
    }
}

/// Human-friendly label for token kinds whose lexeme varies (and so
/// [`TokenKind::as_str`] returns `None`). Used in diagnostics so the
/// message reads naturally.
pub fn token_kind_label(k: TokenKind) -> &'static str {
    match k {
        TokenKind::Ident => "identifier",
        TokenKind::IntLit => "integer literal",
        TokenKind::FloatLit => "float literal",
        TokenKind::StringLit => "string literal",
        TokenKind::RawStringLit => "raw string literal",
        TokenKind::CStringLit => "C string literal",
        TokenKind::RuneLit => "rune literal",
        TokenKind::ByteCharLit => "byte literal",
        TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment => "trivia",
        TokenKind::DocComment => "doc comment",
        TokenKind::Eof => "end of file",
        TokenKind::Error => "invalid token",
        _ => "token",
    }
}
