//! Typed AST views over the CST.
//!
//! See `docs/architecture.md` Part B.3 (CST + AST) and Part C.2 (typed
//! AST as a view over the CST).
//!
//! Every typed AST view is a `Copy` newtype around `&SyntaxNode`. The
//! [`AstNode`] trait provides `cast` (kind-checked construction) and
//! `syntax` (raw access). Accessors on each view filter children by
//! `SyntaxKind`.
//!
//! Phase 0 status: typed views exist for the Phase-1-minimal subset
//! declared in `docs/architecture.md` Part L Phase 1: top-level fns and
//! POD classes, `let`/`if`/`while`/`return`, integer/bool/string
//! literals, basic binary/unary operators. Hooks ([`Item::Stub`]) carry
//! Phase 2+ kinds opaquely until their typed views land.

use crate::cst::{SyntaxElement, SyntaxNode};
use crate::syntax_kind::SyntaxKind;
use arsenal_lex::Span;

/// Trait implemented by every typed AST view.
///
/// `cast` returns `Some` iff the underlying node's kind matches the
/// view's expected kind. Views are `Copy` because they hold only a
/// borrowed pointer.
pub trait AstNode<'a>: Copy + Sized {
    /// Try to view `node` as `Self`; returns `None` if the kind doesn't
    /// match.
    fn cast(node: &'a SyntaxNode<'a>) -> Option<Self>;
    /// Underlying CST node.
    fn syntax(self) -> &'a SyntaxNode<'a>;
    /// Source span of the underlying node.
    fn span(self) -> Span {
        self.syntax().span
    }
}

// ─── helpers ────────────────────────────────────────────────────────────

/// Iterate over the leading [`SyntaxKind::DocComment`] tokens of a node.
/// Stops at the first non-trivia child.
pub fn doc_comments<'a>(node: &'a SyntaxNode<'a>) -> impl Iterator<Item = Span> + 'a {
    node.children
        .iter()
        .take_while(|c| match c {
            SyntaxElement::Token { kind, .. } => kind.is_trivia(),
            SyntaxElement::Node(_) => false,
        })
        .filter_map(|c| match c {
            SyntaxElement::Token {
                kind: SyntaxKind::DocComment,
                span,
            } => Some(*span),
            _ => None,
        })
}

/// Find the first child node castable to `T`.
fn first_child<'a, T: AstNode<'a>>(node: &'a SyntaxNode<'a>) -> Option<T> {
    node.child_nodes().find_map(T::cast)
}

/// Iterate every child node castable to `T`.
fn children<'a, T: AstNode<'a> + 'a>(node: &'a SyntaxNode<'a>) -> impl Iterator<Item = T> + 'a {
    node.child_nodes().filter_map(T::cast)
}

// ─── Module + Items ─────────────────────────────────────────────────────

/// File root. Contains a sequence of [`Item`]s.
#[derive(Copy, Clone)]
pub struct Module<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for Module<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::Module).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> Module<'a> {
    /// All items declared at the top level.
    pub fn items(self) -> impl Iterator<Item = Item<'a>> + 'a {
        self.0.child_nodes().filter_map(Item::cast)
    }

    /// Doc comments attached to the module (leading `///` lines at file
    /// start, before the first item).
    pub fn doc_comments(self) -> impl Iterator<Item = Span> + 'a {
        doc_comments(self.0)
    }
}

/// Top-level item.
///
/// Phase 0 produces typed variants only for [`Item::Fn`] and
/// [`Item::Class`]; Phase 1+ items (`const`, `mod`, `use`, …) are
/// recognised by kind and surfaced through [`Item::Stub`] until their
/// typed views land. [`Item::Error`] holds a parser-recovery placeholder.
#[derive(Copy, Clone)]
pub enum Item<'a> {
    /// `pub? extern? fn ...`
    Fn(FnDecl<'a>),
    /// `pub? class ... { ... }`
    Class(ClassDecl<'a>),
    /// Item kind recognised by the parser but without a typed view yet.
    Stub(&'a SyntaxNode<'a>),
    /// Parser-recovery node.
    Error(&'a SyntaxNode<'a>),
}

impl<'a> Item<'a> {
    /// Try to view `node` as an [`Item`].
    pub fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        Some(match n.kind {
            SyntaxKind::FnDecl => Self::Fn(FnDecl(n)),
            SyntaxKind::ClassDecl => Self::Class(ClassDecl(n)),
            // Phase 1+ item kinds — recognised but not yet typed.
            SyntaxKind::ConstDecl
            | SyntaxKind::ModDecl
            | SyntaxKind::UseDecl
            | SyntaxKind::LibertyDecl
            | SyntaxKind::CipherDecl
            | SyntaxKind::ImplBlock
            | SyntaxKind::AttrItem
            | SyntaxKind::DirectiveItem => Self::Stub(n),
            SyntaxKind::ErrorNode => Self::Error(n),
            _ => return None,
        })
    }

    /// Underlying CST node.
    pub fn syntax(self) -> &'a SyntaxNode<'a> {
        match self {
            Self::Fn(f) => f.syntax(),
            Self::Class(c) => c.syntax(),
            Self::Stub(n) | Self::Error(n) => n,
        }
    }
}

/// `pub? extern? fn name(params) -> ret { body }`.
#[derive(Copy, Clone)]
pub struct FnDecl<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for FnDecl<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::FnDecl).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> FnDecl<'a> {
    /// Function name (`Ident` token following `fn`).
    pub fn name(self) -> Option<Span> {
        self.0.child_token(SyntaxKind::Ident)
    }
    /// Whether the declaration is `pub`.
    pub fn is_pub(self) -> bool {
        self.0.child_token(SyntaxKind::KwPub).is_some()
    }
    /// Whether the declaration is `extern`.
    pub fn is_extern(self) -> bool {
        self.0.child_token(SyntaxKind::KwExtern).is_some()
    }
    /// Parameter list, if present.
    pub fn params(self) -> Option<ParamList<'a>> {
        first_child(self.0)
    }
    /// `-> Type` return annotation, if present.
    pub fn ret_type(self) -> Option<RetType<'a>> {
        first_child(self.0)
    }
    /// Body block, if present (declarations without bodies are valid for
    /// `extern fn`).
    pub fn body(self) -> Option<Block<'a>> {
        first_child(self.0)
    }
    /// Doc comments preceding the declaration.
    pub fn doc_comments(self) -> impl Iterator<Item = Span> + 'a {
        doc_comments(self.0)
    }
}

/// `( p1, p2, ... )`
#[derive(Copy, Clone)]
pub struct ParamList<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for ParamList<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::ParamList).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> ParamList<'a> {
    /// Iterator over the contained parameters.
    pub fn params(self) -> impl Iterator<Item = Param<'a>> + 'a {
        children(self.0)
    }
}

/// `name: ty`
#[derive(Copy, Clone)]
pub struct Param<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for Param<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::Param).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> Param<'a> {
    /// Parameter name.
    pub fn name(self) -> Option<Span> {
        self.0.child_token(SyntaxKind::Ident)
    }
    /// Parameter type annotation.
    pub fn ty(self) -> Option<Type<'a>> {
        self.0.child_nodes().find_map(Type::cast)
    }
}

/// `-> Type`.
#[derive(Copy, Clone)]
pub struct RetType<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for RetType<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::RetType).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> RetType<'a> {
    /// The `Type` after the arrow.
    pub fn ty(self) -> Option<Type<'a>> {
        self.0.child_nodes().find_map(Type::cast)
    }
}

/// `class Name { fields }`.
#[derive(Copy, Clone)]
pub struct ClassDecl<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for ClassDecl<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::ClassDecl).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> ClassDecl<'a> {
    /// Class name (`Ident` token following `class`).
    pub fn name(self) -> Option<Span> {
        self.0.child_token(SyntaxKind::Ident)
    }
    /// Whether the declaration is `pub`.
    pub fn is_pub(self) -> bool {
        self.0.child_token(SyntaxKind::KwPub).is_some()
    }
    /// Field list.
    pub fn fields(self) -> Option<FieldDeclList<'a>> {
        first_child(self.0)
    }
    /// Doc comments preceding the declaration.
    pub fn doc_comments(self) -> impl Iterator<Item = Span> + 'a {
        doc_comments(self.0)
    }
}

/// `{ field, field, ... }` for a class.
#[derive(Copy, Clone)]
pub struct FieldDeclList<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for FieldDeclList<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::FieldDeclList).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> FieldDeclList<'a> {
    /// Iterator over the contained field declarations.
    pub fn fields(self) -> impl Iterator<Item = FieldDecl<'a>> + 'a {
        children(self.0)
    }
}

/// `name: ty (@attr)*`.
#[derive(Copy, Clone)]
pub struct FieldDecl<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for FieldDecl<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::FieldDecl).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> FieldDecl<'a> {
    /// Field name.
    pub fn name(self) -> Option<Span> {
        self.0.child_token(SyntaxKind::Ident)
    }
    /// Field type.
    pub fn ty(self) -> Option<Type<'a>> {
        self.0.child_nodes().find_map(Type::cast)
    }
}

// ─── Statements ─────────────────────────────────────────────────────────

/// Statement node.
#[derive(Copy, Clone)]
pub enum Stmt<'a> {
    /// `let pat: ty = expr;`
    Let(LetStmt<'a>),
    /// `expr;`
    Expr(ExprStmt<'a>),
    /// Recognised but not yet typed (`defer`, `errdefer`, …).
    Stub(&'a SyntaxNode<'a>),
    /// Parser-recovery node.
    Error(&'a SyntaxNode<'a>),
}

impl<'a> Stmt<'a> {
    /// Try to view `node` as a [`Stmt`].
    pub fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        Some(match n.kind {
            SyntaxKind::LetStmt => Self::Let(LetStmt(n)),
            SyntaxKind::ExprStmt => Self::Expr(ExprStmt(n)),
            SyntaxKind::DeferStmt | SyntaxKind::ErrdeferStmt => Self::Stub(n),
            SyntaxKind::ErrorNode => Self::Error(n),
            _ => return None,
        })
    }
}

/// `let pat (: ty)? (= expr)? ;`.
#[derive(Copy, Clone)]
pub struct LetStmt<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for LetStmt<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::LetStmt).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> LetStmt<'a> {
    /// Bound pattern.
    pub fn pattern(self) -> Option<Pattern<'a>> {
        self.0.child_nodes().find_map(Pattern::cast)
    }
    /// Optional type annotation.
    pub fn ty(self) -> Option<Type<'a>> {
        self.0.child_nodes().find_map(Type::cast)
    }
    /// Optional initializer expression.
    pub fn init(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }
}

/// Bare-expression statement: `expr ;`.
#[derive(Copy, Clone)]
pub struct ExprStmt<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for ExprStmt<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::ExprStmt).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> ExprStmt<'a> {
    /// The contained expression.
    pub fn expr(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }
}

// ─── Expressions ────────────────────────────────────────────────────────

/// Expression node.
///
/// Phase 0 typed coverage: literal, path, paren, binary, unary, block,
/// `if`, `while`, `return`, call. Other kinds (Phase 2+) appear as
/// [`Expr::Stub`] until their typed views land.
#[derive(Copy, Clone)]
pub enum Expr<'a> {
    /// Wrapped literal token (int, float, string, true/false, nil, …).
    Literal(LiteralExpr<'a>),
    /// `Foo`, `std::mem::Foo`, `x` (single-segment).
    Path(PathExpr<'a>),
    /// `( expr )`.
    Paren(ParenExpr<'a>),
    /// `lhs OP rhs`.
    Binary(BinaryExpr<'a>),
    /// Prefix unary: `-x`, `!x`, `~x`.
    Unary(UnaryExpr<'a>),
    /// `{ stmt; stmt; tail-expr? }` — block in expression position.
    Block(Block<'a>),
    /// `if cond block (else ...)?`.
    If(IfExpr<'a>),
    /// `while cond block`.
    While(WhileExpr<'a>),
    /// `return` or `return expr`.
    Return(ReturnExpr<'a>),
    /// `callee(args)`.
    Call(CallExpr<'a>),
    /// `break` or `break expr`.
    Break(BreakExpr<'a>),
    /// `continue`.
    Continue(ContinueExpr<'a>),
    /// `for pat in lo..hi { body }` (Phase 1 supports range form only).
    For(ForExpr<'a>),
    /// Recognised expression kind without a typed view yet.
    Stub(&'a SyntaxNode<'a>),
    /// Parser-recovery node.
    Error(&'a SyntaxNode<'a>),
}

impl<'a> Expr<'a> {
    /// Try to view `node` as an [`Expr`].
    pub fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        use SyntaxKind::*;
        Some(match n.kind {
            LiteralExpr => Self::Literal(self::LiteralExpr(n)),
            PathExpr => Self::Path(self::PathExpr(n)),
            ParenExpr => Self::Paren(self::ParenExpr(n)),
            BinaryExpr => Self::Binary(self::BinaryExpr(n)),
            UnaryExpr => Self::Unary(self::UnaryExpr(n)),
            Block => Self::Block(self::Block(n)),
            IfExpr => Self::If(self::IfExpr(n)),
            WhileExpr => Self::While(self::WhileExpr(n)),
            ReturnExpr => Self::Return(self::ReturnExpr(n)),
            CallExpr => Self::Call(self::CallExpr(n)),
            BreakExpr => Self::Break(self::BreakExpr(n)),
            ContinueExpr => Self::Continue(self::ContinueExpr(n)),
            ForExpr => Self::For(self::ForExpr(n)),
            // Hooks
            MatchExpr | LoopExpr | FieldExpr | IndexExpr | CastExpr | RefExpr | DerefExpr
            | RangeExpr | OptionalChainExpr | NilCoalesceExpr | MustExpr | FoxdieExpr
            | CatchExpr | TryExpr | AwaitExpr | YieldExpr | ChannelSendExpr | ChannelRecvExpr
            | LockExpr | NakedExpr | RexBlock | ComptimeExpr | IntrinsicCallExpr
            | AnonAggregateExpr | StructLitExpr | ArrayLitExpr => Self::Stub(n),
            ErrorNode => Self::Error(n),
            _ => return None,
        })
    }

    /// Underlying CST node.
    pub fn syntax(self) -> &'a SyntaxNode<'a> {
        match self {
            Self::Literal(e) => e.syntax(),
            Self::Path(e) => e.syntax(),
            Self::Paren(e) => e.syntax(),
            Self::Binary(e) => e.syntax(),
            Self::Unary(e) => e.syntax(),
            Self::Block(e) => e.syntax(),
            Self::If(e) => e.syntax(),
            Self::While(e) => e.syntax(),
            Self::Return(e) => e.syntax(),
            Self::Call(e) => e.syntax(),
            Self::Break(e) => e.syntax(),
            Self::Continue(e) => e.syntax(),
            Self::For(e) => e.syntax(),
            Self::Stub(n) | Self::Error(n) => n,
        }
    }
}

/// Literal expression: wraps a single literal-token child.
#[derive(Copy, Clone)]
pub struct LiteralExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for LiteralExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::LiteralExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> LiteralExpr<'a> {
    /// Kind and source span of the wrapped literal token.
    ///
    /// This is the **token** span (not the enclosing node's span), so
    /// callers that want to parse the literal text can slice the
    /// source map without picking up trailing trivia.
    pub fn token(self) -> Option<(SyntaxKind, Span)> {
        self.0.children.iter().find_map(|c| match c {
            SyntaxElement::Token { kind, span }
                if matches!(
                    kind,
                    SyntaxKind::IntLit
                        | SyntaxKind::FloatLit
                        | SyntaxKind::StringLit
                        | SyntaxKind::RawStringLit
                        | SyntaxKind::CStringLit
                        | SyntaxKind::RuneLit
                        | SyntaxKind::ByteCharLit
                        | SyntaxKind::KwTrue
                        | SyntaxKind::KwFalse
                        | SyntaxKind::KwNil
                ) =>
            {
                Some((*kind, *span))
            }
            _ => None,
        })
    }

    /// Kind of the wrapped literal token, if any.
    pub fn token_kind(self) -> Option<SyntaxKind> {
        self.token().map(|(k, _)| k)
    }
}

/// Path / identifier reference: `x`, `Foo`, `std::mem::Foo` (multi-segment
/// support is parser-side; the AST view exposes the spans of all `Ident`
/// children in source order).
#[derive(Copy, Clone)]
pub struct PathExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for PathExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::PathExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> PathExpr<'a> {
    /// Spans of each `Ident` segment in source order.
    pub fn segments(self) -> impl Iterator<Item = Span> + 'a {
        self.0.children.iter().filter_map(|c| match c {
            SyntaxElement::Token {
                kind: SyntaxKind::Ident,
                span,
            } => Some(*span),
            _ => None,
        })
    }
}

/// `( expr )`.
#[derive(Copy, Clone)]
pub struct ParenExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for ParenExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::ParenExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> ParenExpr<'a> {
    /// Inner expression.
    pub fn inner(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }
}

/// `lhs OP rhs`.
#[derive(Copy, Clone)]
pub struct BinaryExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for BinaryExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::BinaryExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> BinaryExpr<'a> {
    /// Left operand.
    pub fn lhs(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }
    /// Right operand. Skips the left-hand expression when iterating.
    pub fn rhs(self) -> Option<Expr<'a>> {
        let mut iter = self.0.child_nodes().filter_map(Expr::cast);
        let _lhs = iter.next();
        iter.next()
    }
    /// Operator kind. Returns the kind of the first non-trivia token
    /// child sandwiched between `lhs` and `rhs`.
    pub fn op_kind(self) -> Option<SyntaxKind> {
        // Walk children: find the operator token between the two expr
        // children. The simplest heuristic — first non-trivia token —
        // works because the parser places the operator immediately after
        // the lhs expression node and before any whitespace + rhs.
        let mut after_first_node = false;
        for c in self.0.children {
            match c {
                SyntaxElement::Node(_) => {
                    if !after_first_node {
                        after_first_node = true;
                    } else {
                        return None;
                    }
                }
                SyntaxElement::Token { kind, .. } if after_first_node && !kind.is_trivia() => {
                    return Some(*kind);
                }
                _ => {}
            }
        }
        None
    }
}

/// Prefix unary expression: `-x`, `!x`, `~x`.
#[derive(Copy, Clone)]
pub struct UnaryExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for UnaryExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::UnaryExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> UnaryExpr<'a> {
    /// Operator kind: first non-trivia token child.
    pub fn op_kind(self) -> Option<SyntaxKind> {
        self.0.children.iter().find_map(|c| match c {
            SyntaxElement::Token { kind, .. } if !kind.is_trivia() => Some(*kind),
            _ => None,
        })
    }
    /// Operand.
    pub fn operand(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }
}

/// `{ stmts; tail? }`.
#[derive(Copy, Clone)]
pub struct Block<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for Block<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::Block).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> Block<'a> {
    /// All statement children in source order.
    pub fn stmts(self) -> impl Iterator<Item = Stmt<'a>> + 'a {
        self.0.child_nodes().filter_map(Stmt::cast)
    }
    /// Trailing expression (block as expression form), if the parser
    /// emitted one as the last node child.
    pub fn tail_expr(self) -> Option<Expr<'a>> {
        // Tail is the last child node that is not a statement and not
        // an error. If a block ends with `;` after the tail, the parser
        // emits an `ExprStmt` instead and `tail_expr` returns `None`.
        let mut tail: Option<Expr<'a>> = None;
        for n in self.0.child_nodes() {
            if Stmt::cast(n).is_some() {
                tail = None;
            } else if let Some(e) = Expr::cast(n) {
                tail = Some(e);
            }
        }
        tail
    }
}

/// `if cond block (else ...)?`.
#[derive(Copy, Clone)]
pub struct IfExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for IfExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::IfExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> IfExpr<'a> {
    /// Condition expression — first expression child.
    pub fn cond(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }
    /// Then-branch — first `Block` child.
    pub fn then_block(self) -> Option<Block<'a>> {
        first_child(self.0)
    }
    /// Else branch, either a `Block` (`else { ... }`) or an `IfExpr`
    /// (`else if ...`).
    ///
    /// The parser places the else-arm node *after* the `KwElse` token
    /// in the IfExpr's child list. To distinguish it from the
    /// then-arm, we walk children in source order and return the
    /// first node we see after a `KwElse` token leaf.
    pub fn else_branch(self) -> Option<Expr<'a>> {
        let mut seen_else = false;
        for c in self.0.children {
            match c {
                SyntaxElement::Token {
                    kind: SyntaxKind::KwElse,
                    ..
                } => {
                    seen_else = true;
                }
                SyntaxElement::Node(n) if seen_else => {
                    return Expr::cast(n);
                }
                _ => {}
            }
        }
        None
    }
}

/// `while cond block`.
#[derive(Copy, Clone)]
pub struct WhileExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for WhileExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::WhileExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> WhileExpr<'a> {
    /// Loop condition.
    pub fn cond(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }
    /// Loop body.
    pub fn body(self) -> Option<Block<'a>> {
        first_child(self.0)
    }
}

/// `return` or `return expr`.
#[derive(Copy, Clone)]
pub struct ReturnExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for ReturnExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::ReturnExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> ReturnExpr<'a> {
    /// Optional return value.
    pub fn value(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }
}

/// `callee(arg, arg, ...)`.
#[derive(Copy, Clone)]
pub struct CallExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for CallExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::CallExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> CallExpr<'a> {
    /// Callee expression — first expression child.
    pub fn callee(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }
    /// Argument list, if present.
    pub fn args(self) -> Option<ArgList<'a>> {
        first_child(self.0)
    }
}

/// `(arg, arg, ...)`.
#[derive(Copy, Clone)]
pub struct ArgList<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for ArgList<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::ArgList).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> ArgList<'a> {
    /// Iterator over argument expressions.
    pub fn args(self) -> impl Iterator<Item = Expr<'a>> + 'a {
        self.0.child_nodes().filter_map(Expr::cast)
    }
}

/// `break` or `break expr`.
#[derive(Copy, Clone)]
pub struct BreakExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for BreakExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::BreakExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> BreakExpr<'a> {
    /// Optional break value (Phase 1 doesn't thread it through).
    pub fn value(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }
}

/// `continue`.
#[derive(Copy, Clone)]
pub struct ContinueExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for ContinueExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::ContinueExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

/// `for pat in lo..hi { body }`.
///
/// Phase 1 supports range iterators only; the parser stores the range
/// bounds as two consecutive `Expr` children of the `ForExpr` node (the
/// `..` / `..=` token between them is a leaf token).
#[derive(Copy, Clone)]
pub struct ForExpr<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for ForExpr<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::ForExpr).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> ForExpr<'a> {
    /// Loop variable pattern.
    pub fn pattern(self) -> Option<Pattern<'a>> {
        self.0.child_nodes().find_map(Pattern::cast)
    }

    /// Lower bound of the range (the expression before `..`/`..=`).
    pub fn range_start(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }

    /// Upper bound of the range (the expression after `..`/`..=`).
    pub fn range_end(self) -> Option<Expr<'a>> {
        let mut iter = self.0.child_nodes().filter_map(Expr::cast);
        let _start = iter.next();
        iter.next()
    }

    /// Whether the range is inclusive (`..=`); false for exclusive (`..`).
    pub fn inclusive(self) -> bool {
        self.0.children.iter().any(|c| {
            matches!(
                c,
                SyntaxElement::Token {
                    kind: SyntaxKind::DotDotEq,
                    ..
                }
            )
        })
    }

    /// Loop body block.
    pub fn body(self) -> Option<Block<'a>> {
        first_child(self.0)
    }
}

// ─── Types ──────────────────────────────────────────────────────────────

/// Type expression.
#[derive(Copy, Clone)]
pub enum Type<'a> {
    /// `i32`, `Foo`, `Foo::Bar` — paths in type position.
    Path(PathType<'a>),
    /// `&T`, `&mut T`.
    Ref(RefType<'a>),
    /// `?T`.
    Opt(OptType<'a>),
    /// `[]T`.
    Slice(SliceType<'a>),
    /// `[N]T`.
    Array(ArrayType<'a>),
    /// Recognised but not yet typed.
    Stub(&'a SyntaxNode<'a>),
    /// Parser-recovery node.
    Error(&'a SyntaxNode<'a>),
}

impl<'a> Type<'a> {
    /// Try to view `node` as a [`Type`].
    pub fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        use SyntaxKind::*;
        Some(match n.kind {
            PathType => Self::Path(self::PathType(n)),
            RefType => Self::Ref(self::RefType(n)),
            OptType => Self::Opt(self::OptType(n)),
            SliceType => Self::Slice(self::SliceType(n)),
            ArrayType => Self::Array(self::ArrayType(n)),
            // Hooks
            PtrType | ManyPtrType | SentinelPtrType | ErrorUnionType | TupleType | FnType
            | DynArrayType | GenericArgs => Self::Stub(n),
            ErrorNode => Self::Error(n),
            _ => return None,
        })
    }
}

/// Path type: `i32`, `Foo::Bar`.
#[derive(Copy, Clone)]
pub struct PathType<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for PathType<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::PathType).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> PathType<'a> {
    /// Spans of each `Ident` segment in source order.
    pub fn segments(self) -> impl Iterator<Item = Span> + 'a {
        self.0.children.iter().filter_map(|c| match c {
            SyntaxElement::Token {
                kind: SyntaxKind::Ident,
                span,
            } => Some(*span),
            _ => None,
        })
    }
}

/// Reference type: `&T` or `&mut T`. Mutability detected by presence of
/// `KwVar` (placeholder pending real `mut` keyword decision in Phase 1+);
/// for Phase 0 the parser does not emit `&mut` forms.
#[derive(Copy, Clone)]
pub struct RefType<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for RefType<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::RefType).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> RefType<'a> {
    /// Pointee type.
    pub fn pointee(self) -> Option<Type<'a>> {
        self.0.child_nodes().find_map(Type::cast)
    }
}

/// Optional type: `?T`.
#[derive(Copy, Clone)]
pub struct OptType<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for OptType<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::OptType).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> OptType<'a> {
    /// Inner type.
    pub fn inner(self) -> Option<Type<'a>> {
        self.0.child_nodes().find_map(Type::cast)
    }
}

/// Slice type: `[]T`.
#[derive(Copy, Clone)]
pub struct SliceType<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for SliceType<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::SliceType).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> SliceType<'a> {
    /// Element type.
    pub fn element(self) -> Option<Type<'a>> {
        self.0.child_nodes().find_map(Type::cast)
    }
}

/// Array type: `[N]T`.
#[derive(Copy, Clone)]
pub struct ArrayType<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for ArrayType<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::ArrayType).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> ArrayType<'a> {
    /// Element type.
    pub fn element(self) -> Option<Type<'a>> {
        self.0.child_nodes().find_map(Type::cast)
    }
    /// Length expression — the first expression child appearing inside
    /// the brackets.
    pub fn len_expr(self) -> Option<Expr<'a>> {
        self.0.child_nodes().find_map(Expr::cast)
    }
}

// ─── Patterns ───────────────────────────────────────────────────────────

/// Pattern node.
#[derive(Copy, Clone)]
pub enum Pattern<'a> {
    /// Identifier binding pattern: `name`.
    Ident(IdentPat<'a>),
    /// Wildcard: `_`.
    Wildcard(WildcardPat<'a>),
    /// Recognised but not yet typed (literal patterns, struct patterns, …).
    Stub(&'a SyntaxNode<'a>),
    /// Parser-recovery node.
    Error(&'a SyntaxNode<'a>),
}

impl<'a> Pattern<'a> {
    /// Try to view `node` as a [`Pattern`].
    pub fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        use SyntaxKind::*;
        Some(match n.kind {
            IdentPat => Self::Ident(self::IdentPat(n)),
            WildcardPat => Self::Wildcard(self::WildcardPat(n)),
            LiteralPat | StructPat | TuplePat | OrPat | BindPat => Self::Stub(n),
            ErrorNode => Self::Error(n),
            _ => return None,
        })
    }
}

/// Identifier binding pattern.
#[derive(Copy, Clone)]
pub struct IdentPat<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for IdentPat<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::IdentPat).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

impl<'a> IdentPat<'a> {
    /// Span of the bound identifier.
    pub fn name(self) -> Option<Span> {
        self.0.child_token(SyntaxKind::Ident)
    }
}

/// Wildcard pattern (`_`).
#[derive(Copy, Clone)]
pub struct WildcardPat<'a>(&'a SyntaxNode<'a>);

impl<'a> AstNode<'a> for WildcardPat<'a> {
    fn cast(n: &'a SyntaxNode<'a>) -> Option<Self> {
        (n.kind == SyntaxKind::WildcardPat).then_some(Self(n))
    }
    fn syntax(self) -> &'a SyntaxNode<'a> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::FileArena;
    use crate::cst::CstBuilder;
    use arsenal_lex::{FileId, Span};
    use bumpalo::Bump;

    fn span(start: u32, end: u32) -> Span {
        Span::new(FileId::NONE, start, end)
    }

    /// Build a tiny AST of `fn empty() {}` and exercise the typed views.
    #[test]
    fn fn_decl_view() {
        let bump = Bump::new();
        let arena = FileArena::new(&bump, FileId::NONE);
        let mut b = CstBuilder::new(&arena);

        b.start_node(SyntaxKind::Module, 0);
        b.start_node(SyntaxKind::FnDecl, 0);
        b.push_token(SyntaxKind::KwFn, span(0, 2));
        b.push_token(SyntaxKind::Ident, span(3, 8));
        b.start_node(SyntaxKind::ParamList, 8);
        b.push_token(SyntaxKind::LParen, span(8, 9));
        b.push_token(SyntaxKind::RParen, span(9, 10));
        b.finish_node(10);
        b.start_node(SyntaxKind::Block, 11);
        b.push_token(SyntaxKind::LBrace, span(11, 12));
        b.push_token(SyntaxKind::RBrace, span(12, 13));
        b.finish_node(13);
        b.finish_node(13);
        let root = b.finish_root(13).expect("root");

        let module = Module::cast(root).expect("module");
        let item = module.items().next().expect("one item");
        let f = match item {
            Item::Fn(f) => f,
            _ => panic!("expected Item::Fn"),
        };
        assert_eq!(f.name(), Some(span(3, 8)));
        assert!(!f.is_pub());
        assert!(f.params().is_some());
        assert!(f.body().is_some());
    }

    #[test]
    fn doc_comments_collected() {
        let bump = Bump::new();
        let arena = FileArena::new(&bump, FileId::NONE);
        let mut b = CstBuilder::new(&arena);
        b.start_node(SyntaxKind::Module, 0);
        b.start_node(SyntaxKind::FnDecl, 0);
        b.push_token(SyntaxKind::DocComment, span(0, 10));
        b.push_token(SyntaxKind::Whitespace, span(10, 11));
        b.push_token(SyntaxKind::DocComment, span(11, 21));
        b.push_token(SyntaxKind::Whitespace, span(21, 22));
        b.push_token(SyntaxKind::KwFn, span(22, 24));
        b.push_token(SyntaxKind::Ident, span(25, 29));
        b.finish_node(29);
        let root = b.finish_root(29).expect("root");
        let module = Module::cast(root).expect("module");
        let f = match module.items().next() {
            Some(Item::Fn(f)) => f,
            _ => panic!("expected fn"),
        };
        let docs: Vec<_> = f.doc_comments().collect();
        assert_eq!(docs.len(), 2);
    }

    #[test]
    fn unrecognized_item_is_stub() {
        let bump = Bump::new();
        let arena = FileArena::new(&bump, FileId::NONE);
        let mut b = CstBuilder::new(&arena);
        b.start_node(SyntaxKind::Module, 0);
        b.start_node(SyntaxKind::LibertyDecl, 0);
        b.push_token(SyntaxKind::KwLiberty, span(0, 7));
        b.finish_node(7);
        let root = b.finish_root(7).expect("root");
        let module = Module::cast(root).expect("module");
        match module.items().next() {
            Some(Item::Stub(n)) => assert_eq!(n.kind, SyntaxKind::LibertyDecl),
            _ => panic!("expected Item::Stub for LibertyDecl"),
        };
    }
}
