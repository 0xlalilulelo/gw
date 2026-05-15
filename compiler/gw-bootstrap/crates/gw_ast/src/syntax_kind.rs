//! The unified `SyntaxKind` enum that classifies both leaf tokens and
//! composite nodes in the GW concrete syntax tree.
//!
//! See `docs/architecture.md` Part B.3 (CST + AST) and Part C.2 (typed AST).
//!
//! `SyntaxKind` is a superset of [`gw_lex::TokenKind`]. Every token
//! variant has the same name and semantics on both sides; conversion from
//! `TokenKind` to `SyntaxKind` is provided as a [`From`] impl.
//!
//! Token kinds and node kinds share one enum so that CST nodes and CST
//! tokens both carry a single classifying tag — matching the rust-analyzer
//! / rowan precedent. The token-vs-node distinction is recovered via
//! [`SyntaxKind::is_token`] and [`SyntaxKind::is_node`].

use gw_lex::TokenKind;

/// Every kind of node or token in the GW concrete syntax tree.
///
/// Variants are partitioned in source order: token kinds first, then
/// node kinds. Use [`SyntaxKind::is_token`] / [`SyntaxKind::is_node`] for
/// classification — do not depend on the discriminant numeric ordering.
#[allow(missing_docs)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(u16)]
pub enum SyntaxKind {
    // ────────────────── TOKENS ──────────────────────────────────────
    // Trivia
    Whitespace,
    LineComment,
    BlockComment,
    DocComment,

    // Literals
    IntLit,
    FloatLit,
    StringLit,
    RawStringLit,
    CStringLit,
    RuneLit,
    ByteCharLit,

    // Identifier
    Ident,

    // Keywords
    KwFn,
    KwLet,
    KwVar,
    KwConst,
    KwClass,
    KwMod,
    KwTrait,
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
    KwPub,
    KwUse,
    KwAs,
    KwIn,
    KwWhere,
    KwComptime,
    KwInline,
    KwExtern,
    KwAsm,
    KwLock,
    KwTask,
    KwAwait,
    KwYield,
    KwTrue,
    KwFalse,
    KwNil,
    KwEnum,
    KwUnsafe,
    KwMut,

    // Brackets
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,

    // Punctuation
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
    Arrow,
    FatArrow,
    LArrow,
    At,
    Hash,

    // Synthetic tokens
    Eof,
    /// Lexer error placeholder (renamed from `TokenKind::Error` to avoid
    /// collision with the node-side [`SyntaxKind::ErrorNode`]).
    ErrorTok,

    // ────────────────── NODES ───────────────────────────────────────
    /// File root.
    Module,

    // Items (top-level)
    FnDecl,
    ClassDecl,
    ConstDecl,
    ModDecl,
    UseDecl,

    // Item hooks (Phase 2+; parser produces `ErrorNode` for now)
    TraitDecl,
    ImplBlock,
    AttrItem,
    DirectiveItem,

    // Item internals
    ParamList,
    Param,
    GenericParamList,
    GenericParam,
    RetType,
    WhereClause,
    FieldDeclList,
    FieldDecl,

    // Statements
    LetStmt,
    ExprStmt,
    DeferStmt,
    ErrdeferStmt,

    // Expressions — Phase 1 minimum
    Block,
    LiteralExpr,
    PathExpr,
    BinaryExpr,
    UnaryExpr,
    ParenExpr,
    IfExpr,
    WhileExpr,
    ReturnExpr,
    CallExpr,
    ArgList,

    // Expression hooks (Phase 2+)
    MatchExpr,
    MatchArmList,
    MatchArm,
    ForExpr,
    LoopExpr,
    BreakExpr,
    ContinueExpr,
    FieldExpr,
    IndexExpr,
    CastExpr,
    RefExpr,
    DerefExpr,
    RangeExpr,
    OptionalChainExpr,
    NilCoalesceExpr,
    MustExpr,
    FoxdieExpr,
    CatchExpr,
    TryExpr,
    AwaitExpr,
    YieldExpr,
    ChannelSendExpr,
    ChannelRecvExpr,
    LockExpr,
    NakedExpr,
    RexBlock,
    ComptimeExpr,
    IntrinsicCallExpr,
    AnonAggregateExpr,
    StructLitExpr,
    StructLitFieldList,
    StructLitField,
    ArrayLitExpr,

    // Types — Phase 1 minimum
    PathType,
    RefType,
    OptType,
    SliceType,
    ArrayType,

    // Type hooks (Phase 2+)
    PtrType,
    ManyPtrType,
    SentinelPtrType,
    ErrorUnionType,
    TupleType,
    FnType,
    DynArrayType,
    GenericArgs,

    // Patterns — Phase 1 minimum
    IdentPat,
    WildcardPat,

    // Pattern hooks (Phase 2+)
    LiteralPat,
    /// `lo..=hi` range pattern (Phase 2 increment M.3).
    RangePat,
    StructPat,
    TuplePat,
    OrPat,
    BindPat,

    // Attributes & directives
    Attr,
    AttrArgList,

    // Recovery
    /// Synthetic node emitted by the parser when it cannot determine a
    /// real node kind. Children are whatever the parser had managed to
    /// consume; sibling diagnostics carry the explanation.
    ErrorNode,
}

impl SyntaxKind {
    /// First node-kind variant. Anything `< FIRST_NODE` in source order
    /// is a token; not used for runtime classification (use [`Self::is_token`]).
    pub const FIRST_NODE: SyntaxKind = SyntaxKind::Module;

    /// Whether this kind classifies a leaf token.
    pub const fn is_token(self) -> bool {
        use SyntaxKind::*;
        matches!(
            self,
            Whitespace
                | LineComment
                | BlockComment
                | DocComment
                | IntLit
                | FloatLit
                | StringLit
                | RawStringLit
                | CStringLit
                | RuneLit
                | ByteCharLit
                | Ident
                | KwFn
                | KwLet
                | KwVar
                | KwConst
                | KwClass
                | KwMod
                | KwTrait
                | KwIf
                | KwElse
                | KwMatch
                | KwFor
                | KwWhile
                | KwLoop
                | KwBreak
                | KwContinue
                | KwReturn
                | KwDefer
                | KwErrdefer
                | KwTry
                | KwCatch
                | KwPub
                | KwUse
                | KwAs
                | KwIn
                | KwWhere
                | KwComptime
                | KwInline
                | KwExtern
                | KwAsm
                | KwLock
                | KwTask
                | KwAwait
                | KwYield
                | KwTrue
                | KwFalse
                | KwNil
                | KwEnum
                | KwUnsafe
                | LParen
                | RParen
                | LBrace
                | RBrace
                | LBracket
                | RBracket
                | Comma
                | Semi
                | Colon
                | ColonColon
                | Dot
                | DotDot
                | DotDotEq
                | DotDotDot
                | Question
                | QuestionDot
                | QuestionQ
                | Bang
                | BangBang
                | Eq
                | EqEq
                | BangEq
                | Lt
                | LtEq
                | Gt
                | GtEq
                | Plus
                | Minus
                | Star
                | Slash
                | Percent
                | StarStar
                | PlusEq
                | MinusEq
                | StarEq
                | SlashEq
                | PercentEq
                | StarStarEq
                | Amp
                | AmpAmp
                | Pipe
                | PipePipe
                | Caret
                | Tilde
                | LtLt
                | GtGt
                | AmpEq
                | PipeEq
                | CaretEq
                | LtLtEq
                | GtGtEq
                | Arrow
                | FatArrow
                | LArrow
                | At
                | Hash
                | Eof
                | ErrorTok
        )
    }

    /// Whether this kind classifies a composite node.
    pub const fn is_node(self) -> bool {
        !self.is_token()
    }

    /// Whether this kind classifies trivia (whitespace or any comment kind).
    pub const fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace | Self::LineComment | Self::BlockComment | Self::DocComment
        )
    }
}

impl From<TokenKind> for SyntaxKind {
    fn from(t: TokenKind) -> Self {
        match t {
            TokenKind::Whitespace => Self::Whitespace,
            TokenKind::LineComment => Self::LineComment,
            TokenKind::BlockComment => Self::BlockComment,
            TokenKind::DocComment => Self::DocComment,
            TokenKind::IntLit => Self::IntLit,
            TokenKind::FloatLit => Self::FloatLit,
            TokenKind::StringLit => Self::StringLit,
            TokenKind::RawStringLit => Self::RawStringLit,
            TokenKind::CStringLit => Self::CStringLit,
            TokenKind::RuneLit => Self::RuneLit,
            TokenKind::ByteCharLit => Self::ByteCharLit,
            TokenKind::Ident => Self::Ident,
            TokenKind::KwFn => Self::KwFn,
            TokenKind::KwLet => Self::KwLet,
            TokenKind::KwVar => Self::KwVar,
            TokenKind::KwConst => Self::KwConst,
            TokenKind::KwClass => Self::KwClass,
            TokenKind::KwMod => Self::KwMod,
            TokenKind::KwTrait => Self::KwTrait,
            TokenKind::KwIf => Self::KwIf,
            TokenKind::KwElse => Self::KwElse,
            TokenKind::KwMatch => Self::KwMatch,
            TokenKind::KwFor => Self::KwFor,
            TokenKind::KwWhile => Self::KwWhile,
            TokenKind::KwLoop => Self::KwLoop,
            TokenKind::KwBreak => Self::KwBreak,
            TokenKind::KwContinue => Self::KwContinue,
            TokenKind::KwReturn => Self::KwReturn,
            TokenKind::KwDefer => Self::KwDefer,
            TokenKind::KwErrdefer => Self::KwErrdefer,
            TokenKind::KwTry => Self::KwTry,
            TokenKind::KwCatch => Self::KwCatch,
            TokenKind::KwPub => Self::KwPub,
            TokenKind::KwUse => Self::KwUse,
            TokenKind::KwAs => Self::KwAs,
            TokenKind::KwIn => Self::KwIn,
            TokenKind::KwWhere => Self::KwWhere,
            TokenKind::KwComptime => Self::KwComptime,
            TokenKind::KwInline => Self::KwInline,
            TokenKind::KwExtern => Self::KwExtern,
            TokenKind::KwAsm => Self::KwAsm,
            TokenKind::KwLock => Self::KwLock,
            TokenKind::KwTask => Self::KwTask,
            TokenKind::KwAwait => Self::KwAwait,
            TokenKind::KwYield => Self::KwYield,
            TokenKind::KwTrue => Self::KwTrue,
            TokenKind::KwFalse => Self::KwFalse,
            TokenKind::KwNil => Self::KwNil,
            TokenKind::KwEnum => Self::KwEnum,
            TokenKind::KwUnsafe => Self::KwUnsafe,
            TokenKind::KwMut => Self::KwMut,
            TokenKind::LParen => Self::LParen,
            TokenKind::RParen => Self::RParen,
            TokenKind::LBrace => Self::LBrace,
            TokenKind::RBrace => Self::RBrace,
            TokenKind::LBracket => Self::LBracket,
            TokenKind::RBracket => Self::RBracket,
            TokenKind::Comma => Self::Comma,
            TokenKind::Semi => Self::Semi,
            TokenKind::Colon => Self::Colon,
            TokenKind::ColonColon => Self::ColonColon,
            TokenKind::Dot => Self::Dot,
            TokenKind::DotDot => Self::DotDot,
            TokenKind::DotDotEq => Self::DotDotEq,
            TokenKind::DotDotDot => Self::DotDotDot,
            TokenKind::Question => Self::Question,
            TokenKind::QuestionDot => Self::QuestionDot,
            TokenKind::QuestionQ => Self::QuestionQ,
            TokenKind::Bang => Self::Bang,
            TokenKind::BangBang => Self::BangBang,
            TokenKind::Eq => Self::Eq,
            TokenKind::EqEq => Self::EqEq,
            TokenKind::BangEq => Self::BangEq,
            TokenKind::Lt => Self::Lt,
            TokenKind::LtEq => Self::LtEq,
            TokenKind::Gt => Self::Gt,
            TokenKind::GtEq => Self::GtEq,
            TokenKind::Plus => Self::Plus,
            TokenKind::Minus => Self::Minus,
            TokenKind::Star => Self::Star,
            TokenKind::Slash => Self::Slash,
            TokenKind::Percent => Self::Percent,
            TokenKind::StarStar => Self::StarStar,
            TokenKind::PlusEq => Self::PlusEq,
            TokenKind::MinusEq => Self::MinusEq,
            TokenKind::StarEq => Self::StarEq,
            TokenKind::SlashEq => Self::SlashEq,
            TokenKind::PercentEq => Self::PercentEq,
            TokenKind::StarStarEq => Self::StarStarEq,
            TokenKind::Amp => Self::Amp,
            TokenKind::AmpAmp => Self::AmpAmp,
            TokenKind::Pipe => Self::Pipe,
            TokenKind::PipePipe => Self::PipePipe,
            TokenKind::Caret => Self::Caret,
            TokenKind::Tilde => Self::Tilde,
            TokenKind::LtLt => Self::LtLt,
            TokenKind::GtGt => Self::GtGt,
            TokenKind::AmpEq => Self::AmpEq,
            TokenKind::PipeEq => Self::PipeEq,
            TokenKind::CaretEq => Self::CaretEq,
            TokenKind::LtLtEq => Self::LtLtEq,
            TokenKind::GtGtEq => Self::GtGtEq,
            TokenKind::Arrow => Self::Arrow,
            TokenKind::FatArrow => Self::FatArrow,
            TokenKind::LArrow => Self::LArrow,
            TokenKind::At => Self::At,
            TokenKind::Hash => Self::Hash,
            TokenKind::Eof => Self::Eof,
            TokenKind::Error => Self::ErrorTok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_node_partition() {
        // Spot-check both sides of the boundary.
        assert!(SyntaxKind::Whitespace.is_token());
        assert!(SyntaxKind::Ident.is_token());
        assert!(SyntaxKind::KwFn.is_token());
        assert!(SyntaxKind::Eof.is_token());
        assert!(SyntaxKind::ErrorTok.is_token());
        assert!(!SyntaxKind::Whitespace.is_node());

        assert!(SyntaxKind::Module.is_node());
        assert!(SyntaxKind::FnDecl.is_node());
        assert!(SyntaxKind::IfExpr.is_node());
        assert!(SyntaxKind::ErrorNode.is_node());
        assert!(!SyntaxKind::Module.is_token());
    }

    #[test]
    fn trivia_classification() {
        assert!(SyntaxKind::Whitespace.is_trivia());
        assert!(SyntaxKind::LineComment.is_trivia());
        assert!(SyntaxKind::BlockComment.is_trivia());
        assert!(SyntaxKind::DocComment.is_trivia());
        assert!(!SyntaxKind::Ident.is_trivia());
        assert!(!SyntaxKind::Module.is_trivia());
    }

    #[test]
    fn token_kind_to_syntax_kind_round_trips_token_side() {
        // Every TokenKind maps to a SyntaxKind that is_token().
        let kinds = [
            TokenKind::Whitespace,
            TokenKind::Ident,
            TokenKind::IntLit,
            TokenKind::KwFn,
            TokenKind::Arrow,
            TokenKind::Eof,
            TokenKind::Error,
        ];
        for k in kinds {
            let s: SyntaxKind = k.into();
            assert!(s.is_token(), "{k:?} mapped to non-token {s:?}");
        }
    }

    #[test]
    fn token_kind_error_maps_to_error_tok() {
        let s: SyntaxKind = TokenKind::Error.into();
        assert_eq!(s, SyntaxKind::ErrorTok);
    }
}
