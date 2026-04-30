//! Hand-written UTF-8 lexer state machine on `&[u8]`.
//!
//! See `docs/architecture.md` Part B.2 (lexer architecture) and
//! `docs/spec.md` §5.3 (lexical structure).
//!
//! ## Design
//!
//! - **Input is `&[u8]`** with the precondition that bytes form valid
//!   UTF-8. Source loaded through [`crate::source::SourceMap`] satisfies
//!   this by construction (it stores `String`).
//! - **Identifiers are ASCII-only** (spec §5.3); a non-ASCII byte at an
//!   identifier-start position is a diagnostic, not a silent acceptance.
//! - **Escape sequences inside strings/runes are validated for shape but
//!   not decoded**. Decoding happens at AST construction; the lexer's job
//!   is to identify token boundaries and surface obvious malformedness.
//! - **Trivia is part of the token stream**, not stripped. The parser's
//!   cursor skips trivia on the way to the next significant token; doc
//!   comments are preserved in the CST for `arsenal doc`.
//! - **Errors do not abort lexing**. The lexer emits a `TokenKind::Error`
//!   token covering the bad span, pushes a [`Diagnostic`], and continues
//!   from a recovery point so the parser still sees a usable stream.

use crate::diag::{DiagBag, Diagnostic, ErrorCode, Label};
use crate::keyword::KEYWORDS;
use crate::source::{BytePos, FileId, Span};
use crate::token::{Token, TokenKind};

// Lexer error codes. Reserved range `E0001..E0099`.
const E_UNKNOWN_CHAR: ErrorCode = ErrorCode(1);
const E_UNTERMINATED_STRING: ErrorCode = ErrorCode(2);
const E_UNTERMINATED_BLOCK_COMMENT: ErrorCode = ErrorCode(3);
const E_INVALID_ESCAPE: ErrorCode = ErrorCode(4);
const E_NUMERIC_LITERAL: ErrorCode = ErrorCode(5);
const E_EMPTY_RUNE: ErrorCode = ErrorCode(6);
const E_MULTI_CHAR_RUNE: ErrorCode = ErrorCode(7);
const E_NON_ASCII_IDENT: ErrorCode = ErrorCode(8);
const E_UNTERMINATED_RUNE: ErrorCode = ErrorCode(9);
const E_UNTERMINATED_RAW_STRING: ErrorCode = ErrorCode(10);

/// Lex a source file.
///
/// Returns the token stream — always terminated by a single
/// [`TokenKind::Eof`] token whose span is the empty range at EOF — plus
/// any diagnostics. Trivia tokens (whitespace, comments) are included in
/// the stream.
pub fn lex(file: FileId, src: &[u8]) -> (Vec<Token>, DiagBag) {
    let mut lx = Lexer::new(file, src);
    while !lx.at_eof() {
        let start = lx.pos;
        let kind = lx.next_token();
        let end = lx.pos;
        debug_assert!(
            end > start,
            "lexer made no progress at byte {start} (kind = {kind:?})"
        );
        lx.tokens.push(Token {
            kind,
            span: Span::new(file, start, end),
        });
    }
    let eof_span = Span::new(file, lx.pos, lx.pos);
    lx.tokens.push(Token {
        kind: TokenKind::Eof,
        span: eof_span,
    });
    (lx.tokens, lx.diags)
}

struct Lexer<'src> {
    file: FileId,
    src: &'src [u8],
    pos: BytePos,
    tokens: Vec<Token>,
    diags: DiagBag,
}

impl<'src> Lexer<'src> {
    fn new(file: FileId, src: &'src [u8]) -> Self {
        Self {
            file,
            src,
            pos: 0,
            tokens: Vec::new(),
            diags: DiagBag::new(),
        }
    }

    fn at_eof(&self) -> bool {
        (self.pos as usize) >= self.src.len()
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos as usize).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.src.get((self.pos as usize).saturating_add(1)).copied()
    }

    /// Consume the byte at `self.pos` and return it. The caller must have
    /// verified non-EOF.
    fn bump(&mut self) -> u8 {
        let b = self.src[self.pos as usize];
        self.pos = self.pos.saturating_add(1);
        b
    }

    /// If the next byte equals `b`, consume it and return true.
    fn try_eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos = self.pos.saturating_add(1);
            true
        } else {
            false
        }
    }

    fn span_from(&self, start: BytePos) -> Span {
        Span::new(self.file, start, self.pos)
    }

    fn err(&mut self, code: ErrorCode, span: Span, message: impl Into<String>) {
        self.diags
            .push(Diagnostic::error(code, Label::new(span, ""), message));
    }

    /// Advance `self.pos` past one UTF-8 codepoint, given the current byte
    /// position is at a leader. Bytes are clamped to `src.len()`.
    fn advance_codepoint(&mut self) {
        if let Some(b) = self.peek() {
            let len = utf8_char_len(b);
            let new_pos = (self.pos as usize).saturating_add(len).min(self.src.len()) as BytePos;
            self.pos = new_pos;
        }
    }

    fn next_token(&mut self) -> TokenKind {
        let start = self.pos;
        let b = self.bump();
        match b {
            // Whitespace (spec §5.3: newlines are whitespace).
            b' ' | b'\t' | b'\r' | b'\n' => {
                while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                    self.pos = self.pos.saturating_add(1);
                }
                TokenKind::Whitespace
            }

            // `//`, `///`, `/*`, `/=`, `/`
            b'/' => match self.peek() {
                Some(b'/') => {
                    self.pos = self.pos.saturating_add(1);
                    let is_doc = self.peek() == Some(b'/') && self.peek2() != Some(b'/');
                    if is_doc {
                        self.pos = self.pos.saturating_add(1);
                    }
                    while let Some(p) = self.peek() {
                        if p == b'\n' {
                            break;
                        }
                        self.pos = self.pos.saturating_add(1);
                    }
                    if is_doc {
                        TokenKind::DocComment
                    } else {
                        TokenKind::LineComment
                    }
                }
                Some(b'*') => {
                    self.pos = self.pos.saturating_add(1);
                    self.lex_block_comment(start)
                }
                Some(b'=') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::SlashEq
                }
                _ => TokenKind::Slash,
            },

            // String literal `"..."`
            b'"' => self.lex_string(start, /* c_str */ false),

            // Rune literal `'A'`
            b'\'' => self.lex_rune(start, /* byte */ false),

            // `\\...\\` raw string. Single `\` is an unknown character.
            b'\\' => {
                if self.peek() == Some(b'\\') {
                    self.pos = self.pos.saturating_add(1);
                    self.lex_raw_string(start)
                } else {
                    self.unknown_char(start, b)
                }
            }

            // Numeric literal.
            b'0'..=b'9' => self.lex_number(start, b),

            // `c"..."`, `c'A'`, or just an identifier starting with `c`.
            b'c' => match self.peek() {
                Some(b'"') => {
                    self.pos = self.pos.saturating_add(1);
                    self.lex_string(start, true)
                }
                Some(b'\'') => {
                    self.pos = self.pos.saturating_add(1);
                    self.lex_rune(start, true)
                }
                _ => self.lex_ident_or_keyword(start),
            },

            // Identifier / keyword.
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.lex_ident_or_keyword(start),

            // Brackets.
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,

            // Punctuation.
            b',' => TokenKind::Comma,
            b';' => TokenKind::Semi,
            b':' => {
                if self.try_eat(b':') {
                    TokenKind::ColonColon
                } else {
                    TokenKind::Colon
                }
            }
            b'.' => match self.peek() {
                Some(b'.') => {
                    self.pos = self.pos.saturating_add(1);
                    if self.try_eat(b'=') {
                        TokenKind::DotDotEq
                    } else if self.try_eat(b'.') {
                        TokenKind::DotDotDot
                    } else {
                        TokenKind::DotDot
                    }
                }
                _ => TokenKind::Dot,
            },
            b'?' => match self.peek() {
                Some(b'.') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::QuestionDot
                }
                Some(b'?') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::QuestionQ
                }
                _ => TokenKind::Question,
            },
            b'!' => match self.peek() {
                Some(b'=') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::BangEq
                }
                Some(b'!') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::BangBang
                }
                _ => TokenKind::Bang,
            },
            b'=' => match self.peek() {
                Some(b'=') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::EqEq
                }
                Some(b'>') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::FatArrow
                }
                _ => TokenKind::Eq,
            },
            b'<' => match self.peek() {
                Some(b'=') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::LtEq
                }
                Some(b'<') => {
                    self.pos = self.pos.saturating_add(1);
                    if self.try_eat(b'=') {
                        TokenKind::LtLtEq
                    } else {
                        TokenKind::LtLt
                    }
                }
                Some(b'-') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::LArrow
                }
                _ => TokenKind::Lt,
            },
            b'>' => match self.peek() {
                Some(b'=') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::GtEq
                }
                Some(b'>') => {
                    self.pos = self.pos.saturating_add(1);
                    if self.try_eat(b'=') {
                        TokenKind::GtGtEq
                    } else {
                        TokenKind::GtGt
                    }
                }
                _ => TokenKind::Gt,
            },
            b'+' => {
                if self.try_eat(b'=') {
                    TokenKind::PlusEq
                } else {
                    TokenKind::Plus
                }
            }
            b'-' => match self.peek() {
                Some(b'=') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::MinusEq
                }
                Some(b'>') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::Arrow
                }
                _ => TokenKind::Minus,
            },
            b'*' => match self.peek() {
                Some(b'*') => {
                    self.pos = self.pos.saturating_add(1);
                    if self.try_eat(b'=') {
                        TokenKind::StarStarEq
                    } else {
                        TokenKind::StarStar
                    }
                }
                Some(b'=') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::StarEq
                }
                _ => TokenKind::Star,
            },
            b'%' => {
                if self.try_eat(b'=') {
                    TokenKind::PercentEq
                } else {
                    TokenKind::Percent
                }
            }
            b'&' => match self.peek() {
                Some(b'&') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::AmpAmp
                }
                Some(b'=') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::AmpEq
                }
                _ => TokenKind::Amp,
            },
            b'|' => match self.peek() {
                Some(b'|') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::PipePipe
                }
                Some(b'=') => {
                    self.pos = self.pos.saturating_add(1);
                    TokenKind::PipeEq
                }
                _ => TokenKind::Pipe,
            },
            b'^' => {
                if self.try_eat(b'=') {
                    TokenKind::CaretEq
                } else {
                    TokenKind::Caret
                }
            }
            b'~' => TokenKind::Tilde,
            b'@' => TokenKind::At,
            b'#' => TokenKind::Hash,

            _ => self.unknown_char(start, b),
        }
    }

    // ────── Helpers ─────────────────────────────────────────────────────

    fn lex_block_comment(&mut self, start: BytePos) -> TokenKind {
        // Opening `/*` already consumed.
        let mut depth: u32 = 1;
        while depth > 0 {
            match self.peek() {
                None => {
                    let span = self.span_from(start);
                    self.err(
                        E_UNTERMINATED_BLOCK_COMMENT,
                        span,
                        "unterminated block comment",
                    );
                    return TokenKind::Error;
                }
                Some(b'*') => {
                    self.pos = self.pos.saturating_add(1);
                    if self.try_eat(b'/') {
                        depth -= 1;
                    }
                }
                Some(b'/') => {
                    self.pos = self.pos.saturating_add(1);
                    if self.try_eat(b'*') {
                        depth = depth.saturating_add(1);
                    }
                }
                Some(_) => self.advance_codepoint(),
            }
        }
        TokenKind::BlockComment
    }

    fn lex_string(&mut self, start: BytePos, c_str: bool) -> TokenKind {
        // Opening `"` already consumed (or `c"`).
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    let span = self.span_from(start);
                    let msg = if c_str {
                        "unterminated C string literal"
                    } else {
                        "unterminated string literal"
                    };
                    self.err(E_UNTERMINATED_STRING, span, msg);
                    return TokenKind::Error;
                }
                Some(b'"') => {
                    self.pos = self.pos.saturating_add(1);
                    break;
                }
                Some(b'\\') => {
                    self.pos = self.pos.saturating_add(1);
                    self.consume_escape();
                }
                Some(_) => self.advance_codepoint(),
            }
        }
        if c_str {
            TokenKind::CStringLit
        } else {
            TokenKind::StringLit
        }
    }

    fn lex_raw_string(&mut self, start: BytePos) -> TokenKind {
        // Opening `\\` already consumed.
        loop {
            match self.peek() {
                None => {
                    let span = self.span_from(start);
                    self.err(
                        E_UNTERMINATED_RAW_STRING,
                        span,
                        "unterminated raw string literal",
                    );
                    return TokenKind::Error;
                }
                Some(b'\\') => {
                    self.pos = self.pos.saturating_add(1);
                    if self.try_eat(b'\\') {
                        return TokenKind::RawStringLit;
                    }
                    // Single backslash inside raw string is just data.
                }
                Some(_) => self.advance_codepoint(),
            }
        }
    }

    fn lex_rune(&mut self, start: BytePos, byte: bool) -> TokenKind {
        // Opening `'` already consumed.
        match self.peek() {
            None | Some(b'\n') => {
                let span = self.span_from(start);
                let msg = if byte {
                    "unterminated byte literal"
                } else {
                    "unterminated rune literal"
                };
                self.err(E_UNTERMINATED_RUNE, span, msg);
                return TokenKind::Error;
            }
            Some(b'\'') => {
                // Empty `''` or `c''`.
                self.pos = self.pos.saturating_add(1);
                let span = self.span_from(start);
                let msg = if byte {
                    "empty byte literal"
                } else {
                    "empty rune literal"
                };
                self.err(E_EMPTY_RUNE, span, msg);
                return TokenKind::Error;
            }
            Some(b'\\') => {
                self.pos = self.pos.saturating_add(1);
                self.consume_escape();
            }
            Some(_) => self.advance_codepoint(),
        }
        // Now expect closing `'`.
        if !self.try_eat(b'\'') {
            // Multi-char or unterminated. Consume until `'`, `\n`, or EOF.
            loop {
                match self.peek() {
                    None | Some(b'\n') => {
                        let span = self.span_from(start);
                        let msg = if byte {
                            "unterminated byte literal"
                        } else {
                            "unterminated rune literal"
                        };
                        self.err(E_UNTERMINATED_RUNE, span, msg);
                        return TokenKind::Error;
                    }
                    Some(b'\'') => {
                        self.pos = self.pos.saturating_add(1);
                        break;
                    }
                    Some(b'\\') => {
                        self.pos = self.pos.saturating_add(1);
                        self.consume_escape();
                    }
                    Some(_) => self.advance_codepoint(),
                }
            }
            let span = self.span_from(start);
            let msg = if byte {
                "byte literal must contain a single ASCII character"
            } else {
                "rune literal must contain a single Unicode scalar value"
            };
            self.err(E_MULTI_CHAR_RUNE, span, msg);
            return TokenKind::Error;
        }
        if byte {
            TokenKind::ByteCharLit
        } else {
            TokenKind::RuneLit
        }
    }

    /// Consume one escape sequence, given the leading `\` was already
    /// eaten. On malformed escapes this pushes a diagnostic and returns
    /// without advancing further than the bad bytes.
    fn consume_escape(&mut self) {
        match self.peek() {
            None => {
                // EOF after `\`. The enclosing string/rune handler will
                // surface the unterminated-literal error.
            }
            Some(b'n' | b't' | b'r' | b'\\' | b'\'' | b'"' | b'0') => {
                self.pos = self.pos.saturating_add(1);
            }
            Some(b'x') => {
                let escape_start = self.pos.saturating_sub(1);
                self.pos = self.pos.saturating_add(1);
                for _ in 0..2 {
                    if !matches!(self.peek(), Some(b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F')) {
                        let span = Span::new(self.file, escape_start, self.pos);
                        self.err(
                            E_INVALID_ESCAPE,
                            span,
                            "invalid hex escape: expected two hex digits after `\\x`",
                        );
                        return;
                    }
                    self.pos = self.pos.saturating_add(1);
                }
            }
            Some(b'u') => {
                let escape_start = self.pos.saturating_sub(1);
                self.pos = self.pos.saturating_add(1);
                if !self.try_eat(b'{') {
                    let span = Span::new(self.file, escape_start, self.pos);
                    self.err(
                        E_INVALID_ESCAPE,
                        span,
                        "invalid Unicode escape: expected `\\u{...}`",
                    );
                    return;
                }
                let hex_start = self.pos;
                while matches!(self.peek(), Some(b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F')) {
                    self.pos = self.pos.saturating_add(1);
                }
                if self.pos == hex_start {
                    let span = Span::new(self.file, escape_start, self.pos);
                    self.err(E_INVALID_ESCAPE, span, "empty Unicode escape `\\u{}`");
                }
                if !self.try_eat(b'}') {
                    let span = Span::new(self.file, escape_start, self.pos);
                    self.err(
                        E_INVALID_ESCAPE,
                        span,
                        "expected `}` to close Unicode escape",
                    );
                }
            }
            Some(_) => {
                let escape_start = self.pos.saturating_sub(1);
                self.advance_codepoint();
                let span = Span::new(self.file, escape_start, self.pos);
                self.err(E_INVALID_ESCAPE, span, "unknown escape sequence");
            }
        }
    }

    fn lex_ident_or_keyword(&mut self, start: BytePos) -> TokenKind {
        // First byte already consumed.
        loop {
            match self.peek() {
                Some(b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_') => {
                    self.pos = self.pos.saturating_add(1);
                }
                Some(b) if b >= 0x80 => {
                    // Non-ASCII byte mid-identifier. Consume the codepoint
                    // and emit a diagnostic, then keep going so we still
                    // produce a usable Ident token.
                    let cp_start = self.pos;
                    self.advance_codepoint();
                    let span = Span::new(self.file, cp_start, self.pos);
                    self.err(
                        E_NON_ASCII_IDENT,
                        span,
                        "identifiers must be ASCII; non-ASCII characters are not allowed",
                    );
                }
                _ => break,
            }
        }
        // Keyword lookup against the ASCII portion. If non-ASCII bytes
        // were rejected above, the slice may still contain them; the phf
        // table lookup returns None for any non-keyword text.
        let bytes = &self.src[start as usize..self.pos as usize];
        if let Ok(text) = std::str::from_utf8(bytes) {
            if let Some(kind) = KEYWORDS.get(text).copied() {
                return kind;
            }
        }
        TokenKind::Ident
    }

    fn lex_number(&mut self, start: BytePos, first: u8) -> TokenKind {
        // Base-prefixed integer (`0x..`, `0o..`, `0b..`) or hex float.
        if first == b'0' {
            match self.peek() {
                Some(b'x' | b'X') => {
                    self.pos = self.pos.saturating_add(1);
                    let digits_start = self.pos;
                    self.eat_hex_digits();
                    let had_int_part = self.pos > digits_start;
                    // Hex float? `0x[hex]+ . [hex]+ p [+-]? [dec]+`
                    let is_hex_float = self.peek() == Some(b'.')
                        && matches!(self.peek2(), Some(b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F'));
                    if is_hex_float {
                        self.pos = self.pos.saturating_add(1);
                        self.eat_hex_digits();
                    }
                    if matches!(self.peek(), Some(b'p' | b'P')) {
                        self.pos = self.pos.saturating_add(1);
                        self.eat_exp_sign_and_digits();
                        if !had_int_part && !is_hex_float {
                            let span = self.span_from(start);
                            self.err(
                                E_NUMERIC_LITERAL,
                                span,
                                "hex float literal must contain at least one digit before the exponent",
                            );
                        }
                        return TokenKind::FloatLit;
                    }
                    if is_hex_float {
                        return TokenKind::FloatLit;
                    }
                    if !had_int_part {
                        let span = self.span_from(start);
                        self.err(
                            E_NUMERIC_LITERAL,
                            span,
                            "hex literal must contain at least one digit",
                        );
                    }
                    return TokenKind::IntLit;
                }
                Some(b'o' | b'O') => {
                    self.pos = self.pos.saturating_add(1);
                    let dstart = self.pos;
                    self.eat_octal_digits();
                    if self.pos == dstart {
                        let span = self.span_from(start);
                        self.err(
                            E_NUMERIC_LITERAL,
                            span,
                            "octal literal must contain at least one digit",
                        );
                    }
                    return TokenKind::IntLit;
                }
                Some(b'b' | b'B') => {
                    self.pos = self.pos.saturating_add(1);
                    let dstart = self.pos;
                    self.eat_bin_digits();
                    if self.pos == dstart {
                        let span = self.span_from(start);
                        self.err(
                            E_NUMERIC_LITERAL,
                            span,
                            "binary literal must contain at least one digit",
                        );
                    }
                    return TokenKind::IntLit;
                }
                _ => { /* fall through to decimal */ }
            }
        }
        // Decimal int / float.
        self.eat_dec_digits();
        let mut is_float = false;
        if self.peek() == Some(b'.') && matches!(self.peek2(), Some(b'0'..=b'9')) {
            is_float = true;
            self.pos = self.pos.saturating_add(1);
            self.eat_dec_digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos = self.pos.saturating_add(1);
            self.eat_exp_sign_and_digits();
        }
        if is_float {
            TokenKind::FloatLit
        } else {
            TokenKind::IntLit
        }
    }

    fn eat_dec_digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9' | b'_')) {
            self.pos = self.pos.saturating_add(1);
        }
    }

    fn eat_hex_digits(&mut self) {
        while matches!(
            self.peek(),
            Some(b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' | b'_')
        ) {
            self.pos = self.pos.saturating_add(1);
        }
    }

    fn eat_octal_digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'7' | b'_')) {
            self.pos = self.pos.saturating_add(1);
        }
    }

    fn eat_bin_digits(&mut self) {
        while matches!(self.peek(), Some(b'0' | b'1' | b'_')) {
            self.pos = self.pos.saturating_add(1);
        }
    }

    fn eat_exp_sign_and_digits(&mut self) {
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos = self.pos.saturating_add(1);
        }
        self.eat_dec_digits();
    }

    fn unknown_char(&mut self, start: BytePos, b: u8) -> TokenKind {
        if b >= 0x80 {
            // We've already consumed one byte; advance to the end of the
            // multi-byte codepoint.
            let want_len = utf8_char_len(b);
            let target = (start as usize)
                .saturating_add(want_len)
                .min(self.src.len()) as BytePos;
            if target > self.pos {
                self.pos = target;
            }
        }
        let span = Span::new(self.file, start, self.pos);
        self.err(E_UNKNOWN_CHAR, span, "unexpected character");
        TokenKind::Error
    }
}

/// Length, in bytes, of the UTF-8 codepoint that begins with `leader`.
///
/// Bytes in `0x80..0xC0` are continuation bytes and never appear at a leader
/// position in valid UTF-8; this function returns `1` for them so the lexer
/// can advance past the bad byte without infinite-looping (callers will have
/// already emitted a diagnostic).
const fn utf8_char_len(leader: u8) -> usize {
    if leader < 0xC0 {
        // ASCII or stray continuation byte — both consume 1 byte.
        1
    } else if leader < 0xE0 {
        2
    } else if leader < 0xF0 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::TokenKind::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let (tokens, _) = lex(FileId(1), src.as_bytes());
        tokens.into_iter().map(|t| t.kind).collect()
    }

    fn nontrivia(src: &str) -> Vec<TokenKind> {
        kinds(src).into_iter().filter(|k| !k.is_trivia()).collect()
    }

    fn errors(src: &str) -> u32 {
        let (_, diag) = lex(FileId(1), src.as_bytes());
        diag.error_count()
    }

    #[test]
    fn empty_input_emits_just_eof() {
        assert_eq!(kinds(""), vec![Eof]);
    }

    #[test]
    fn whitespace_runs_collapse() {
        assert_eq!(kinds("  \t\n  "), vec![Whitespace, Eof]);
    }

    #[test]
    fn keywords_and_punct() {
        assert_eq!(
            nontrivia("fn foo() -> u0 { return 0; }"),
            vec![
                KwFn, Ident, LParen, RParen, Arrow, Ident, LBrace, KwReturn, IntLit, Semi, RBrace,
                Eof,
            ]
        );
    }

    #[test]
    fn line_and_doc_comments() {
        let v = kinds("// hi\n/// doc\n//// not doc\nfn");
        let line_count = v.iter().filter(|&&k| k == LineComment).count();
        let doc_count = v.iter().filter(|&&k| k == DocComment).count();
        assert_eq!(line_count, 2, "// and //// are line comments");
        assert_eq!(doc_count, 1, "/// is a doc comment");
    }

    #[test]
    fn nested_block_comments() {
        let (toks, diag) = lex(FileId(1), b"/* a /* b */ c */");
        assert!(!diag.has_errors());
        assert!(toks.iter().any(|t| t.kind == BlockComment));
    }

    #[test]
    fn unterminated_block_comment() {
        assert_eq!(errors("/* never closed"), 1);
    }

    #[test]
    fn strings_and_runes() {
        // r#"..."# is a Rust raw string; the `\\raw\\` inside is GW raw-string syntax.
        let src = r#""hi" 'A' c"x" c'y' \\raw\\"#;
        assert_eq!(
            nontrivia(src),
            vec![
                StringLit,
                RuneLit,
                CStringLit,
                ByteCharLit,
                RawStringLit,
                Eof,
            ]
        );
    }

    #[test]
    fn string_escapes_well_formed() {
        assert_eq!(errors(r#""line\nbreak\thash\x41done""#), 0);
        assert_eq!(errors(r#""unicode \u{1F600}""#), 0);
    }

    #[test]
    fn string_invalid_escape_diagnoses() {
        assert_eq!(errors(r#""bad \q escape""#), 1);
    }

    #[test]
    fn unterminated_string() {
        assert_eq!(errors("\"abc"), 1);
        // Newline ends the first string with an error; lexing resumes and
        // the second `"` opens a new string that is then unterminated by EOF.
        // Two distinct errors is the right answer.
        assert_eq!(errors("\"abc\ndef\""), 2);
    }

    #[test]
    fn rune_basic() {
        assert_eq!(
            nontrivia("'A' '\\n' '\\u{41}'"),
            vec![RuneLit, RuneLit, RuneLit, Eof]
        );
    }

    #[test]
    fn rune_multi_char_diagnoses() {
        assert!(errors("'AB'") >= 1);
    }

    #[test]
    fn rune_empty_diagnoses() {
        assert_eq!(errors("''"), 1);
        assert_eq!(errors("c''"), 1);
    }

    #[test]
    fn numbers() {
        assert_eq!(
            nontrivia("0 0xFF 0o7 0b10 1_000 3.14 1e9 0x1.fp10"),
            vec![IntLit, IntLit, IntLit, IntLit, IntLit, FloatLit, FloatLit, FloatLit, Eof,]
        );
    }

    #[test]
    fn integer_dot_method_call_not_float() {
        // `1.foo()` is `IntLit Dot Ident LParen RParen`, not `FloatLit`.
        assert_eq!(
            nontrivia("1.foo()"),
            vec![IntLit, Dot, Ident, LParen, RParen, Eof]
        );
    }

    #[test]
    fn empty_hex_diagnoses() {
        assert_eq!(errors("0x"), 1);
        assert_eq!(errors("0o"), 1);
        assert_eq!(errors("0b"), 1);
    }

    #[test]
    fn three_char_punct() {
        assert_eq!(
            nontrivia(":: .. ..= ... ?? ?. !! -> => <- == != <= >= ** **= <<= >>="),
            vec![
                ColonColon,
                DotDot,
                DotDotEq,
                DotDotDot,
                QuestionQ,
                QuestionDot,
                BangBang,
                Arrow,
                FatArrow,
                LArrow,
                EqEq,
                BangEq,
                LtEq,
                GtEq,
                StarStar,
                StarStarEq,
                LtLtEq,
                GtGtEq,
                Eof,
            ]
        );
    }

    #[test]
    fn single_punct() {
        assert_eq!(
            nontrivia("+ - * / % & | ^ ~ < > = ! ? . , ; : @ #"),
            vec![
                Plus, Minus, Star, Slash, Percent, Amp, Pipe, Caret, Tilde, Lt, Gt, Eq, Bang,
                Question, Dot, Comma, Semi, Colon, At, Hash, Eof,
            ]
        );
    }

    #[test]
    fn compound_assigns() {
        assert_eq!(
            nontrivia("+= -= *= /= %= &= |= ^="),
            vec![PlusEq, MinusEq, StarEq, SlashEq, PercentEq, AmpEq, PipeEq, CaretEq, Eof,]
        );
    }

    #[test]
    fn unknown_char_emits_error() {
        // `$` is not a GW token (HolyC's DolDoc syntax is removed per spec §5.3).
        assert!(errors("$") >= 1);
    }

    #[test]
    fn non_ascii_in_ident_diagnoses() {
        // 'ñ' is non-ASCII
        let src = "let x\u{00F1} = 1;";
        let (_, diag) = lex(FileId(1), src.as_bytes());
        assert!(diag.error_count() >= 1);
    }

    #[test]
    fn span_round_trip_concatenates_to_source() {
        let src = "fn f() { return 42; }";
        let (toks, _) = lex(FileId(1), src.as_bytes());
        let mut reconstructed = String::new();
        for t in &toks {
            if t.kind == Eof {
                break;
            }
            let s = t.span.start as usize;
            let e = t.span.end as usize;
            reconstructed.push_str(&src[s..e]);
        }
        assert_eq!(reconstructed, src);
    }

    #[test]
    fn raw_string_preserves_newlines() {
        let src = "\\\\multi\nline\\\\";
        assert_eq!(nontrivia(src), vec![RawStringLit, Eof]);
    }

    #[test]
    fn theme_keywords_recognized() {
        assert_eq!(
            nontrivia("liberty cipher foxdie naked enum union unsafe"),
            vec![KwLiberty, KwCipher, KwFoxdie, KwNaked, KwEnum, KwUnion, KwUnsafe, Eof,]
        );
    }
}
