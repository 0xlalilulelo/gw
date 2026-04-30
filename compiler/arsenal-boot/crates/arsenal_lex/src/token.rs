//! Token stream produced by the GW lexer.
//!
//! See `docs/spec.md` §5.3 (lexical structure) and `docs/architecture.md`
//! Part B.2 (lexer architecture).

use crate::source::Span;

/// A single token: kind plus source span.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Token {
    /// Kind of token.
    pub kind: TokenKind,
    /// Source span this token covers.
    pub span: Span,
}

/// Every token kind GW recognises at the lexer level.
///
/// `TokenKind` is payload-free: literal base/value, escape sequences, and
/// keyword text are recovered by re-reading the span at AST construction
/// time. This keeps tokens `Copy` and 1 byte, which makes incremental
/// relexing and CST allocation cheap.
///
/// Trivia variants ([`TokenKind::Whitespace`], the comment kinds) appear in
/// the token stream and are skipped by the parser cursor; doc comments are
/// preserved as CST trivia for `arsenal doc`.
#[allow(missing_docs)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum TokenKind {
    // ───────── Trivia ────────────────────────────────────────────────
    /// Spaces, tabs, and newlines (spec §5.3: "Newlines are whitespace").
    Whitespace,
    /// `// line comment\n`.
    LineComment,
    /// `/* block comment */`. Block comments may be nested.
    BlockComment,
    /// `/// doc comment` — preserved as trivia attached to the following item.
    DocComment,

    // ───────── Literals ──────────────────────────────────────────────
    /// `123`, `0xFF`, `0o755`, `0b1010`, `1_000_000`.
    IntLit,
    /// `3.14`, `1.0e9`, `0x1.fp10`.
    FloatLit,
    /// `"..."` UTF-8 string with escape sequences.
    StringLit,
    /// `\\multi-line raw string\\` (spec §5.3).
    RawStringLit,
    /// `c"..."` null-terminated C string.
    CStringLit,
    /// `'A'` Unicode scalar value (rune, u32).
    RuneLit,
    /// `c'A'` byte char (u8).
    ByteCharLit,

    // ───────── Identifier ────────────────────────────────────────────
    /// `[A-Za-z_][A-Za-z0-9_]*`. ASCII-only at the lexer level.
    Ident,

    // ───────── Keywords (spec §5.3) ──────────────────────────────────
    KwFn,
    KwLet,
    KwVar,
    KwConst,
    KwClass,
    KwLiberty,
    KwCipher,
    KwIf,
    KwElse,
    KwMatch,
    KwFor,
    KwWhile,
    KwLoop,
    KwBreak,
    KwContinue,
    KwReturn,
    KwDefer,
    KwErrdefer,
    KwTry,
    KwCatch,
    KwFoxdie,
    KwNaked,
    KwPub,
    KwMod,
    KwUse,
    KwAs,
    KwIn,
    KwWhere,
    KwComptime,
    KwInline,
    KwExtern,
    KwRex,
    KwLock,
    KwFox,
    KwAwait,
    KwYield,
    KwTrue,
    KwFalse,
    KwNil,

    // Reserved theme aliases — rejected as identifiers so that `liberty` ↔
    // `enum union` aliasing (spec §5.4.3) and `unsafe` ↔ `naked` aliasing
    // (spec §5.5.1) can be wired up later without source-breaking changes.
    KwEnum,
    KwUnion,
    KwUnsafe,

    // ───────── Brackets ──────────────────────────────────────────────
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,

    // ───────── Punctuation / operators ───────────────────────────────
    Comma,
    Semi,
    Colon,
    ColonColon,
    Dot,
    DotDot,
    DotDotEq,
    DotDotDot,

    Question,
    QuestionDot,
    QuestionQ,
    Bang,
    BangBang,

    Eq,
    EqEq,
    BangEq,
    Lt,
    LtEq,
    Gt,
    GtEq,

    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    StarStarEq,

    Amp,
    AmpAmp,
    Pipe,
    PipePipe,
    Caret,
    Tilde,
    LtLt,
    GtGt,
    AmpEq,
    PipeEq,
    CaretEq,
    LtLtEq,
    GtGtEq,

    /// `->` function return type.
    Arrow,
    /// `=>` match arm.
    FatArrow,
    /// `<-` channel send.
    LArrow,

    /// `@` comptime intrinsic prefix (`@codec`, `@field`, `@call`).
    At,
    /// `#` attribute / directive prefix (`#[..]`, `#virtuous`, `#run`).
    Hash,

    // ───────── Synthetic ─────────────────────────────────────────────
    /// End-of-file sentinel.
    Eof,
    /// Lexer error placeholder. Diagnostic emitted alongside; the parser
    /// treats `Error` tokens as opaque during recovery.
    Error,
}

impl TokenKind {
    /// Whether this token is trivia (whitespace or any comment kind).
    pub const fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace | Self::LineComment | Self::BlockComment | Self::DocComment
        )
    }

    /// Whether this token is a keyword (including reserved theme aliases).
    pub const fn is_keyword(self) -> bool {
        matches!(
            self,
            Self::KwFn
                | Self::KwLet
                | Self::KwVar
                | Self::KwConst
                | Self::KwClass
                | Self::KwLiberty
                | Self::KwCipher
                | Self::KwIf
                | Self::KwElse
                | Self::KwMatch
                | Self::KwFor
                | Self::KwWhile
                | Self::KwLoop
                | Self::KwBreak
                | Self::KwContinue
                | Self::KwReturn
                | Self::KwDefer
                | Self::KwErrdefer
                | Self::KwTry
                | Self::KwCatch
                | Self::KwFoxdie
                | Self::KwNaked
                | Self::KwPub
                | Self::KwMod
                | Self::KwUse
                | Self::KwAs
                | Self::KwIn
                | Self::KwWhere
                | Self::KwComptime
                | Self::KwInline
                | Self::KwExtern
                | Self::KwRex
                | Self::KwLock
                | Self::KwFox
                | Self::KwAwait
                | Self::KwYield
                | Self::KwTrue
                | Self::KwFalse
                | Self::KwNil
                | Self::KwEnum
                | Self::KwUnion
                | Self::KwUnsafe
        )
    }

    /// Whether this token is one of the seven literal kinds.
    pub const fn is_literal(self) -> bool {
        matches!(
            self,
            Self::IntLit
                | Self::FloatLit
                | Self::StringLit
                | Self::RawStringLit
                | Self::CStringLit
                | Self::RuneLit
                | Self::ByteCharLit
        )
    }

    /// Static text for tokens whose lexeme is fixed. Returns `None` for
    /// tokens whose text varies (identifiers, literals, trivia, `Eof`,
    /// `Error`).
    ///
    /// Useful for diagnostic messages and tests.
    pub const fn as_str(self) -> Option<&'static str> {
        Some(match self {
            // Keywords
            Self::KwFn => "fn",
            Self::KwLet => "let",
            Self::KwVar => "var",
            Self::KwConst => "const",
            Self::KwClass => "class",
            Self::KwLiberty => "liberty",
            Self::KwCipher => "cipher",
            Self::KwIf => "if",
            Self::KwElse => "else",
            Self::KwMatch => "match",
            Self::KwFor => "for",
            Self::KwWhile => "while",
            Self::KwLoop => "loop",
            Self::KwBreak => "break",
            Self::KwContinue => "continue",
            Self::KwReturn => "return",
            Self::KwDefer => "defer",
            Self::KwErrdefer => "errdefer",
            Self::KwTry => "try",
            Self::KwCatch => "catch",
            Self::KwFoxdie => "foxdie",
            Self::KwNaked => "naked",
            Self::KwPub => "pub",
            Self::KwMod => "mod",
            Self::KwUse => "use",
            Self::KwAs => "as",
            Self::KwIn => "in",
            Self::KwWhere => "where",
            Self::KwComptime => "comptime",
            Self::KwInline => "inline",
            Self::KwExtern => "extern",
            Self::KwRex => "rex",
            Self::KwLock => "lock",
            Self::KwFox => "fox",
            Self::KwAwait => "await",
            Self::KwYield => "yield",
            Self::KwTrue => "true",
            Self::KwFalse => "false",
            Self::KwNil => "nil",
            Self::KwEnum => "enum",
            Self::KwUnion => "union",
            Self::KwUnsafe => "unsafe",
            // Brackets
            Self::LParen => "(",
            Self::RParen => ")",
            Self::LBrace => "{",
            Self::RBrace => "}",
            Self::LBracket => "[",
            Self::RBracket => "]",
            // Punctuation
            Self::Comma => ",",
            Self::Semi => ";",
            Self::Colon => ":",
            Self::ColonColon => "::",
            Self::Dot => ".",
            Self::DotDot => "..",
            Self::DotDotEq => "..=",
            Self::DotDotDot => "...",
            Self::Question => "?",
            Self::QuestionDot => "?.",
            Self::QuestionQ => "??",
            Self::Bang => "!",
            Self::BangBang => "!!",
            Self::Eq => "=",
            Self::EqEq => "==",
            Self::BangEq => "!=",
            Self::Lt => "<",
            Self::LtEq => "<=",
            Self::Gt => ">",
            Self::GtEq => ">=",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Star => "*",
            Self::Slash => "/",
            Self::Percent => "%",
            Self::StarStar => "**",
            Self::PlusEq => "+=",
            Self::MinusEq => "-=",
            Self::StarEq => "*=",
            Self::SlashEq => "/=",
            Self::PercentEq => "%=",
            Self::StarStarEq => "**=",
            Self::Amp => "&",
            Self::AmpAmp => "&&",
            Self::Pipe => "|",
            Self::PipePipe => "||",
            Self::Caret => "^",
            Self::Tilde => "~",
            Self::LtLt => "<<",
            Self::GtGt => ">>",
            Self::AmpEq => "&=",
            Self::PipeEq => "|=",
            Self::CaretEq => "^=",
            Self::LtLtEq => "<<=",
            Self::GtGtEq => ">>=",
            Self::Arrow => "->",
            Self::FatArrow => "=>",
            Self::LArrow => "<-",
            Self::At => "@",
            Self::Hash => "#",
            // Variable-text or synthetic
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifications() {
        assert!(TokenKind::Whitespace.is_trivia());
        assert!(TokenKind::DocComment.is_trivia());
        assert!(!TokenKind::Ident.is_trivia());
        assert!(TokenKind::KwFn.is_keyword());
        assert!(!TokenKind::Ident.is_keyword());
        assert!(TokenKind::IntLit.is_literal());
        assert!(!TokenKind::KwTrue.is_literal()); // bool literal goes through KwTrue/KwFalse
    }

    #[test]
    fn as_str_round_trip() {
        assert_eq!(TokenKind::KwFn.as_str(), Some("fn"));
        assert_eq!(TokenKind::FatArrow.as_str(), Some("=>"));
        assert_eq!(TokenKind::DotDotEq.as_str(), Some("..="));
        assert_eq!(TokenKind::Ident.as_str(), None);
        assert_eq!(TokenKind::Eof.as_str(), None);
    }
}
