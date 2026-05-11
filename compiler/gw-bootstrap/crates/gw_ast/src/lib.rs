//! GW concrete syntax tree (CST) and typed abstract syntax tree (AST).
//!
//! See `docs/architecture.md` Part B.3 (CST + AST split) and Part C.2
//! (typed AST for type checking, comptime, and reflection).
//!
//! Phase 0 status: data structures and Phase-1-minimal typed views land
//! here; the parser ([`gw_parse`]) builds CSTs through [`CstBuilder`]
//! and the driver dumps them via [`print::dump`].

pub mod arena;
pub mod ast;
pub mod cst;
pub mod print;
pub mod syntax_kind;

pub use arena::FileArena;
pub use ast::{
    ArgList, ArrayType, AstNode, BinaryExpr, Block, BreakExpr, CallExpr, CastExpr, ClassDecl,
    ContinueExpr, ErrorUnionType, Expr, ExprStmt, FieldDecl, FieldDeclList, FieldExpr, FnDecl,
    ForExpr, IdentPat, IfExpr, Item, LetStmt, LibertyDecl, LiteralExpr, LiteralPat, MatchArm,
    MatchArmList, MatchExpr, Module, MustExpr, OptType, OrPat, Param, ParamList, ParenExpr,
    PathExpr, PathType, Pattern, RangePat, RefType, RetType, ReturnExpr, SliceType, Stmt,
    StructLitExpr, StructLitField, StructLitFieldList, Type, UnaryExpr, UseDecl, WhileExpr,
    WildcardPat,
};
pub use cst::{Checkpoint, CstBuilder, SyntaxElement, SyntaxNode};
pub use print::{dump, dump_with, DumpOpts};
pub use syntax_kind::SyntaxKind;
