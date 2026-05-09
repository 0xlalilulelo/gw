//! Recursive-descent grammar rules with Pratt-style expression
//! precedence.
//!
//! See `docs/architecture.md` Part B.3 (parser) and the Phase-1 minimum
//! subset declared in Part L Phase 1: top-level fns, POD classes,
//! `let`/`if`/`while`/`return`, integer/bool/string literals, and basic
//! binary/unary operators.
//!
//! Items not in the Phase-1 minimum subset (`liberty`, `cipher`, `match`,
//! `for`, async, comptime, etc.) are recognised by leading keyword and
//! either:
//!   - emitted as `ErrorNode` plus a "not yet supported" diagnostic, or
//!   - skipped using bracket-counted recovery to the next item boundary.
//!
//! The CST is lossless: every token (including trivia) is emitted into
//! the tree exactly once.

use crate::parser::{ec, Parser};
use crate::recovery::{is_item_start, is_stmt_boundary};
use arsenal_ast::SyntaxKind;
use arsenal_lex::TokenKind;

// ─── public entry point ────────────────────────────────────────────────

/// Parse a [`SyntaxKind::Module`] frame. Opens the frame, parses items
/// and top-level statements until EOF, drains any trailing trivia, but
/// does **not** call `finish_root` — the caller is responsible for that
/// so it can recover the root reference.
///
/// A top-level form is classified by the leading keyword (after any
/// `pub`/`extern` modifiers): `fn`, `class`, and other recognised item
/// keywords are parsed as items; anything else is parsed as a
/// statement, which downstream passes collect into a synthetic `main`
/// (Phase 1 increment 11a — see `docs/HANDOFF.md`).
///
/// Recovery is best-effort; the result is always a syntactically-shaped
/// Module node, possibly containing `ErrorNode` children where recovery
/// had to discard tokens.
pub fn parse_module(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::Module, start);
    while !p.at(TokenKind::Eof) {
        let before = p.pos;
        if is_top_level_item_kind(peek_item_keyword(p)) {
            parse_item_or_recover(p);
        } else {
            parse_stmt(p);
        }
        if p.pos == before {
            // Defensive: don't infinite-loop on a sync-set member.
            // Bump one significant token under an Error node.
            recover_one_token(p);
        }
    }
    // Trailing trivia goes into Module so the dump stays lossless.
    p.skip_trivia_into_node();
}

/// Whether a leading keyword (post `pub`/`extern` modifiers) starts a
/// module item. Anything else at the top level is a top-level statement.
/// `Eof` routes through the item path so a stray `pub`/`extern` with
/// nothing after it produces the existing "expected an item" diagnostic.
fn is_top_level_item_kind(kw: TokenKind) -> bool {
    matches!(
        kw,
        TokenKind::KwFn
            | TokenKind::KwClass
            | TokenKind::KwLiberty
            | TokenKind::KwCipher
            | TokenKind::KwConst
            | TokenKind::KwMod
            | TokenKind::KwUse
            | TokenKind::KwEnum
            | TokenKind::KwUnion
            | TokenKind::KwInline
            | TokenKind::KwComptime
            | TokenKind::Hash
            | TokenKind::At
            | TokenKind::Eof
    )
}

fn recover_one_token(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::ErrorNode, start);
    if !p.at(TokenKind::Eof) {
        p.bump_any();
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

// ─── items ─────────────────────────────────────────────────────────────

fn parse_item_or_recover(p: &mut Parser<'_, '_, '_>) {
    // Look past trivia, `pub`, `extern` modifiers to determine item kind.
    let kw = peek_item_keyword(p);
    let item_start = p.cur_byte_start();
    match kw {
        TokenKind::KwFn => parse_fn_decl(p, item_start),
        TokenKind::KwClass => parse_class_decl(p, item_start),
        // Recognised but not supported in Phase 0.
        TokenKind::KwLiberty
        | TokenKind::KwCipher
        | TokenKind::KwConst
        | TokenKind::KwMod
        | TokenKind::KwUse
        | TokenKind::KwEnum
        | TokenKind::KwUnion
        | TokenKind::Hash
        | TokenKind::At => recover_unsupported_item(p, item_start, kw),
        _ => recover_unrecognized_item(p, item_start),
    }
}

/// Look at the first significant token after any leading `pub` /
/// `extern` modifiers. Returns the kind of the keyword that decides what
/// item kind to parse.
fn peek_item_keyword(p: &Parser<'_, '_, '_>) -> TokenKind {
    let mut offset = 0;
    let mut kind = p.peek_at(offset);
    while matches!(kind, TokenKind::KwPub | TokenKind::KwExtern) {
        offset += 1;
        kind = p.peek_at(offset);
    }
    kind
}

fn recover_unsupported_item(p: &mut Parser<'_, '_, '_>, start: u32, kw: TokenKind) {
    let span = p.current_span();
    let label = kw.as_str().unwrap_or("<keyword>");
    p.error(
        ec::EXPECTED_ITEM,
        span,
        format!("`{label}` items are not yet supported by the Phase 0 parser"),
    );
    p.builder.start_node(SyntaxKind::ErrorNode, start);
    // Consume the leading keyword unconditionally so that
    // `skip_to_item_boundary` doesn't stop instantly on the same token
    // (item-start keywords are sync-set members for the *next* item).
    if !p.at(TokenKind::Eof) {
        p.bump_any();
    }
    skip_to_item_boundary(p);
    // For terminating-`;` items (`mod foo;`, `use std::mem;`, `const X = 1;`),
    // also consume any trailing `;` so the next iteration starts cleanly.
    let _ = p.eat(TokenKind::Semi);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn recover_unrecognized_item(p: &mut Parser<'_, '_, '_>, start: u32) {
    p.unexpected("an item (`fn`, `class`, …)");
    p.builder.start_node(SyntaxKind::ErrorNode, start);
    // Consume at least one token so we make progress.
    if !p.at(TokenKind::Eof) {
        p.bump_any();
    }
    skip_to_item_boundary(p);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

/// Bracket-counted skip: consume tokens (emitting them into the open
/// node) until we reach an item-start keyword at depth 0, or EOF.
fn skip_to_item_boundary(p: &mut Parser<'_, '_, '_>) {
    let mut depth: i32 = 0;
    loop {
        let k = p.current();
        if k == TokenKind::Eof {
            return;
        }
        if depth == 0 && is_item_start(k) {
            return;
        }
        match k {
            TokenKind::LParen | TokenKind::LBrace | TokenKind::LBracket => depth += 1,
            TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                if depth == 0 {
                    // Don't eat a closing brace that would belong to a
                    // surrounding scope.
                    return;
                }
                depth -= 1;
            }
            _ => {}
        }
        p.bump_any();
    }
}

/// Same idea but for statement-level recovery: skip until `;`, `}`, or
/// an item-start keyword at depth 0.
fn skip_to_stmt_boundary(p: &mut Parser<'_, '_, '_>) {
    let mut depth: i32 = 0;
    loop {
        let k = p.current();
        if k == TokenKind::Eof {
            return;
        }
        if depth == 0 && is_stmt_boundary(k) {
            return;
        }
        match k {
            TokenKind::LParen | TokenKind::LBrace | TokenKind::LBracket => depth += 1,
            TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                if depth == 0 {
                    return;
                }
                depth -= 1;
            }
            _ => {}
        }
        p.bump_any();
    }
}

// ─── fn decl ───────────────────────────────────────────────────────────

fn parse_fn_decl(p: &mut Parser<'_, '_, '_>, start: u32) {
    p.builder.start_node(SyntaxKind::FnDecl, start);
    // Drain leading trivia (doc comments) into FnDecl.
    p.skip_trivia_into_node();
    // Modifiers
    let _ = p.eat(TokenKind::KwPub);
    let _ = p.eat(TokenKind::KwExtern);
    // `fn`
    p.expect(TokenKind::KwFn);
    // Name
    p.expect(TokenKind::Ident);
    // Params
    if p.at(TokenKind::LParen) {
        parse_param_list(p);
    } else {
        p.unexpected("`(` to begin parameter list");
    }
    // Optional return type
    if p.at(TokenKind::Arrow) {
        parse_ret_type(p);
    }
    // Body or terminating `;` (extern fn forms).
    if p.at(TokenKind::LBrace) {
        parse_block(p);
    } else if p.at(TokenKind::Semi) {
        p.bump_any();
    } else {
        p.unexpected("`{` to begin function body or `;` to end an extern declaration");
        skip_to_stmt_boundary(p);
        let _ = p.eat(TokenKind::Semi);
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_param_list(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::ParamList, start);
    p.expect(TokenKind::LParen);
    while !p.at(TokenKind::RParen) && !p.at(TokenKind::Eof) {
        parse_param(p);
        if !p.eat(TokenKind::Comma) {
            break;
        }
    }
    p.expect(TokenKind::RParen);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_param(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::Param, start);
    if !p.expect(TokenKind::Ident) {
        // Recovery: skip until `,` or `)`.
        while !p.at_any(&[TokenKind::Comma, TokenKind::RParen, TokenKind::Eof]) {
            p.bump_any();
        }
        let end = p.cur_byte_start();
        p.builder.finish_node(end);
        return;
    }
    if p.eat(TokenKind::Colon) {
        parse_type(p);
    } else {
        p.unexpected("`:` followed by a type annotation");
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_ret_type(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::RetType, start);
    p.expect(TokenKind::Arrow);
    parse_type(p);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

// ─── class decl ────────────────────────────────────────────────────────

fn parse_class_decl(p: &mut Parser<'_, '_, '_>, start: u32) {
    p.builder.start_node(SyntaxKind::ClassDecl, start);
    p.skip_trivia_into_node();
    let _ = p.eat(TokenKind::KwPub);
    p.expect(TokenKind::KwClass);
    p.expect(TokenKind::Ident);
    if p.at(TokenKind::LBrace) {
        parse_field_decl_list(p);
    } else {
        p.unexpected("`{` to begin class body");
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_field_decl_list(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::FieldDeclList, start);
    p.expect(TokenKind::LBrace);
    while !p.at(TokenKind::RBrace) && !p.at(TokenKind::Eof) {
        parse_field_decl(p);
        if !p.eat(TokenKind::Comma) {
            break;
        }
    }
    p.expect(TokenKind::RBrace);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_field_decl(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::FieldDecl, start);
    if !p.expect(TokenKind::Ident) {
        // Recovery: stop at `,` or `}`.
        while !p.at_any(&[TokenKind::Comma, TokenKind::RBrace, TokenKind::Eof]) {
            p.bump_any();
        }
        let end = p.cur_byte_start();
        p.builder.finish_node(end);
        return;
    }
    if p.eat(TokenKind::Colon) {
        parse_type(p);
    } else {
        p.unexpected("`:` followed by a type annotation");
    }
    // Phase 0 deliberately skips field metadata (`@range(...)`); the
    // lexer already produced the tokens, recovery skips them.
    while p.at(TokenKind::At) {
        skip_attr(p);
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn skip_attr(p: &mut Parser<'_, '_, '_>) {
    // Eat `@` ... up to and including a balanced `(...)` if present.
    p.bump_any(); // @
    if p.at(TokenKind::Ident) {
        p.bump_any();
    }
    if p.at(TokenKind::LParen) {
        let mut depth = 1;
        p.bump_any();
        while depth > 0 && !p.at(TokenKind::Eof) {
            match p.current() {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => depth -= 1,
                _ => {}
            }
            p.bump_any();
        }
    }
}

// ─── types ─────────────────────────────────────────────────────────────

fn parse_type(p: &mut Parser<'_, '_, '_>) {
    let k = p.current();
    let start = p.cur_byte_start();
    match k {
        TokenKind::Amp => {
            p.builder.start_node(SyntaxKind::RefType, start);
            p.bump_any(); // &
                          // `&mut T` — `mut` is not a token in Phase 0; ignore.
            parse_type(p);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
        TokenKind::Question => {
            p.builder.start_node(SyntaxKind::OptType, start);
            p.bump_any(); // ?
            parse_type(p);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
        TokenKind::Star => {
            // `*T` — raw pointer (spec §5.4). Phase 1 only accepts
            // `*u8`/`*i8` element types; typeck enforces the restriction.
            p.builder.start_node(SyntaxKind::PtrType, start);
            p.bump_any(); // *
            parse_type(p);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
        TokenKind::Bang => {
            // `!T` — error union (Phase 2 increment O.3). Phase-2
            // minimum realises an anonymous-error union over
            // primitive `T`; richer payload types and named errors
            // ride later sub-bundles.
            p.builder.start_node(SyntaxKind::ErrorUnionType, start);
            p.bump_any(); // !
            parse_type(p);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
        TokenKind::LBracket => {
            // `[]T` (slice), `[*:S]T` (sentinel many-pointer, Phase 2
            // C.2), or `[N]T` (array). Decide by peeking past the
            // opening bracket's significant tokens.
            let inner = p.peek_at(1);
            if inner == TokenKind::RBracket {
                // SliceType
                p.builder.start_node(SyntaxKind::SliceType, start);
                p.bump_any(); // [
                p.bump_any(); // ]
                parse_type(p);
                let end = p.cur_byte_start();
                p.builder.finish_node(end);
            } else if inner == TokenKind::Star {
                // SentinelPtrType: `[ * : <expr> ] T`. Parser accepts
                // any sentinel expression here; typeck restricts it to
                // a `0` literal in Phase 2 (only `[*:0]u8` is wired up).
                p.builder.start_node(SyntaxKind::SentinelPtrType, start);
                p.bump_any(); // [
                p.bump_any(); // *
                p.expect(TokenKind::Colon);
                parse_expr(p);
                p.expect(TokenKind::RBracket);
                parse_type(p);
                let end = p.cur_byte_start();
                p.builder.finish_node(end);
            } else {
                // ArrayType
                p.builder.start_node(SyntaxKind::ArrayType, start);
                p.bump_any(); // [
                parse_expr(p);
                p.expect(TokenKind::RBracket);
                parse_type(p);
                let end = p.cur_byte_start();
                p.builder.finish_node(end);
            }
        }
        TokenKind::Ident => {
            p.builder.start_node(SyntaxKind::PathType, start);
            p.bump_any(); // first segment
            while p.at(TokenKind::ColonColon) {
                p.bump_any();
                p.expect(TokenKind::Ident);
            }
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
        _ => {
            let span = p.current_span();
            p.error(
                ec::EXPECTED_TYPE,
                span,
                format!(
                    "expected type, found `{}`",
                    k.as_str().unwrap_or(crate::parser::token_kind_label(k)),
                ),
            );
            p.builder.start_node(SyntaxKind::ErrorNode, start);
            // Don't bump — caller's recovery will handle it.
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
    }
}

// ─── statements ────────────────────────────────────────────────────────

fn parse_block(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::Block, start);
    p.expect(TokenKind::LBrace);
    while !p.at(TokenKind::RBrace) && !p.at(TokenKind::Eof) {
        let before = p.pos;
        parse_stmt(p);
        if p.pos == before {
            // No progress: emit one error token and recover.
            p.unexpected("a statement");
            if !p.at_any(&[TokenKind::RBrace, TokenKind::Eof]) {
                p.bump_any();
            }
        }
    }
    p.expect(TokenKind::RBrace);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_stmt(p: &mut Parser<'_, '_, '_>) {
    match p.current() {
        TokenKind::KwLet => parse_let_stmt(p),
        TokenKind::KwReturn => parse_expr_stmt(p, /* leading_kw */ true),
        TokenKind::KwIf | TokenKind::KwWhile | TokenKind::KwFor | TokenKind::LBrace => {
            // Block-like expressions used as statements; they may stand
            // without a trailing `;`.
            let start = p.cur_byte_start();
            p.builder.start_node(SyntaxKind::ExprStmt, start);
            parse_expr(p);
            let _ = p.eat(TokenKind::Semi);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
        // Items are NOT permitted inside a block in Phase 0.
        _ => parse_expr_stmt(p, false),
    }
}

fn parse_let_stmt(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::LetStmt, start);
    p.expect(TokenKind::KwLet);
    parse_pattern(p);
    if p.eat(TokenKind::Colon) {
        parse_type(p);
    }
    if p.eat(TokenKind::Eq) {
        parse_expr(p);
    }
    if !p.expect(TokenKind::Semi) {
        skip_to_stmt_boundary(p);
        let _ = p.eat(TokenKind::Semi);
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_expr_stmt(p: &mut Parser<'_, '_, '_>, _leading_kw: bool) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::ExprStmt, start);
    parse_expr(p);
    if !p.expect(TokenKind::Semi) {
        skip_to_stmt_boundary(p);
        let _ = p.eat(TokenKind::Semi);
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

// ─── patterns ──────────────────────────────────────────────────────────

fn parse_pattern(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    match p.current() {
        TokenKind::Ident => {
            // `_` is lexed as a regular Ident; classify it here.
            let span = p.current_span();
            let is_underscore = p.span_bytes(span) == b"_";
            let kind = if is_underscore {
                SyntaxKind::WildcardPat
            } else {
                SyntaxKind::IdentPat
            };
            p.builder.start_node(kind, start);
            p.bump_any();
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
        _ => {
            let span = p.current_span();
            p.error(
                ec::EXPECTED_PATTERN,
                span,
                "expected a pattern (identifier or `_`)",
            );
            p.builder.start_node(SyntaxKind::ErrorNode, start);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
    }
}

// ─── expressions (Pratt) ───────────────────────────────────────────────

/// Binding power tuple: `(left_bp, right_bp)`.
///
/// Left-associative operators have `left < right` (so a parser at
/// `right_bp` won't eat another op at the same precedence level).
/// Right-associative operators reverse it.
fn infix_bp(op: TokenKind) -> Option<(u8, u8)> {
    Some(match op {
        // Assignment is right-associative and binds the loosest of any
        // infix, so `x = y + 1` parses as `x = (y + 1)` and
        // `a = b = c` parses as `a = (b = c)`.
        TokenKind::Eq => (2, 1),
        TokenKind::PipePipe => (3, 4),
        TokenKind::AmpAmp => (5, 6),
        TokenKind::EqEq | TokenKind::BangEq => (7, 8),
        TokenKind::Lt | TokenKind::LtEq | TokenKind::Gt | TokenKind::GtEq => (7, 8),
        // `??` (Phase 2 increment O.1) — right-associative, tighter
        // than logical / comparison / bitwise, looser than arithmetic.
        // Right-assoc so `a ?? b ?? c` chains as `a ?? (b ?? c)`.
        TokenKind::QuestionQ => (16, 15),
        TokenKind::Pipe => (9, 10),
        TokenKind::Caret => (11, 12),
        TokenKind::Amp => (13, 14),
        TokenKind::LtLt | TokenKind::GtGt => (15, 16),
        TokenKind::Plus | TokenKind::Minus => (17, 18),
        TokenKind::Star | TokenKind::Slash | TokenKind::Percent => (19, 20),
        // `**` is right-associative.
        TokenKind::StarStar => (22, 21),
        _ => return None,
    })
}

/// Returns the unary-operator binding power (right-side only — prefix
/// unary has no left side). `None` means the token is not a unary
/// operator.
fn prefix_bp(op: TokenKind) -> Option<u8> {
    Some(match op {
        TokenKind::Minus | TokenKind::Bang | TokenKind::Tilde => 23,
        _ => return None,
    })
}

/// Left binding power of the postfix `as` cast.
///
/// Sits between multiplicative (`*`/`/`/`%` at 19/20, `**` at 22/21) and
/// prefix unary (23): tighter than `*` so `a * b as T` parses as
/// `a * (b as T)`, but looser than unary so `-1 as u32` parses as
/// `(-1) as u32`. Same precedence as Rust.
const AS_CAST_BP: u8 = 22;

fn parse_expr(p: &mut Parser<'_, '_, '_>) {
    parse_expr_bp(p, 0);
}

fn parse_expr_bp(p: &mut Parser<'_, '_, '_>, min_bp: u8) {
    // Drain leading trivia *into the parent node* before we take a
    // checkpoint, otherwise the wrap might pull trivia into a
    // BinaryExpr it doesn't belong to.
    p.skip_trivia_into_node();
    let cp = p.builder.checkpoint();
    let lhs_start = p.cur_byte_start();

    parse_atom(p);

    loop {
        // Postfix: function call.
        if p.at(TokenKind::LParen) {
            p.builder.start_node_at(cp, SyntaxKind::CallExpr, lhs_start);
            parse_arg_list(p);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
            continue;
        }

        // Postfix: field access `.name`. Distinguished from float
        // literals at the lexer level — `1.5` is a single FloatLit
        // token, while `v.x` lexes as Ident Dot Ident.
        if p.at(TokenKind::Dot) && p.peek_at(1) == TokenKind::Ident {
            p.builder
                .start_node_at(cp, SyntaxKind::FieldExpr, lhs_start);
            p.bump_any(); // .
            p.expect(TokenKind::Ident);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
            continue;
        }

        // Postfix: `expr!` "must-be-ok" assert (Phase 2 increment
        // O.3). Distinct from prefix `!bool` (logical not, handled
        // in `parse_atom`). The lexer collapses `!=` into a single
        // `BangEq` token, so a bare `Bang` here unambiguously means
        // postfix-assert.
        if p.at(TokenKind::Bang) {
            p.builder.start_node_at(cp, SyntaxKind::MustExpr, lhs_start);
            p.bump_any(); // !
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
            continue;
        }

        // Postfix: `as Type` value cast. Bound by `AS_CAST_BP` so it
        // respects the surrounding precedence; the operand is
        // whatever the lhs already parsed and the right side is a
        // type, not an expression.
        if p.at(TokenKind::KwAs) {
            if AS_CAST_BP < min_bp {
                break;
            }
            p.builder.start_node_at(cp, SyntaxKind::CastExpr, lhs_start);
            p.bump_any(); // `as`
            parse_type(p);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
            continue;
        }

        let op = p.current();
        let Some((l_bp, r_bp)) = infix_bp(op) else {
            break;
        };
        if l_bp < min_bp {
            break;
        }

        p.builder
            .start_node_at(cp, SyntaxKind::BinaryExpr, lhs_start);
        p.bump_any(); // operator
        parse_expr_bp(p, r_bp);
        let end = p.cur_byte_start();
        p.builder.finish_node(end);
    }
}

fn parse_struct_lit_field_list(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::StructLitFieldList, start);
    p.expect(TokenKind::LBrace);
    while !p.at(TokenKind::RBrace) && !p.at(TokenKind::Eof) {
        parse_struct_lit_field(p);
        if !p.eat(TokenKind::Comma) {
            break;
        }
    }
    p.expect(TokenKind::RBrace);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_struct_lit_field(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::StructLitField, start);
    p.expect(TokenKind::Dot);
    p.expect(TokenKind::Ident);
    p.expect(TokenKind::Eq);
    parse_expr(p);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_atom(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    let k = p.current();
    if let Some(bp) = prefix_bp(k) {
        p.builder.start_node(SyntaxKind::UnaryExpr, start);
        p.bump_any();
        parse_expr_bp(p, bp);
        let end = p.cur_byte_start();
        p.builder.finish_node(end);
        return;
    }
    match k {
        TokenKind::IntLit
        | TokenKind::FloatLit
        | TokenKind::StringLit
        | TokenKind::RawStringLit
        | TokenKind::CStringLit
        | TokenKind::RuneLit
        | TokenKind::ByteCharLit
        | TokenKind::KwTrue
        | TokenKind::KwFalse
        | TokenKind::KwNil => {
            p.builder.start_node(SyntaxKind::LiteralExpr, start);
            p.bump_any();
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
        TokenKind::Ident => {
            // Take a checkpoint scoped to just the PathExpr so we can
            // retroactively wrap it in a StructLitExpr if a `{` follows.
            let path_cp = p.builder.checkpoint();
            p.builder.start_node(SyntaxKind::PathExpr, start);
            p.bump_any();
            while p.at(TokenKind::ColonColon) {
                p.bump_any();
                p.expect(TokenKind::Ident);
            }
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
            // Struct literal continuation: `Foo { .x = 1, .y = 2 }`.
            // Suppressed inside `if`/`while`/`for` conditions where the
            // `{` belongs to the enclosing block (see `parse_cond_expr`).
            if p.struct_literals_allowed && p.at(TokenKind::LBrace) {
                p.builder
                    .start_node_at(path_cp, SyntaxKind::StructLitExpr, start);
                parse_struct_lit_field_list(p);
                let end = p.cur_byte_start();
                p.builder.finish_node(end);
            }
        }
        TokenKind::LParen => {
            p.builder.start_node(SyntaxKind::ParenExpr, start);
            p.bump_any();
            parse_expr(p);
            p.expect(TokenKind::RParen);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
        TokenKind::LBrace => {
            parse_block(p);
        }
        TokenKind::KwIf => parse_if_expr(p),
        TokenKind::KwWhile => parse_while_expr(p),
        TokenKind::KwReturn => parse_return_expr(p),
        TokenKind::KwBreak => parse_break_expr(p),
        TokenKind::KwContinue => parse_continue_expr(p),
        TokenKind::KwFor => parse_for_expr(p),
        TokenKind::KwMatch => parse_match_expr(p),
        _ => {
            let span = p.current_span();
            p.error(
                ec::EXPECTED_EXPR,
                span,
                format!(
                    "expected an expression, found `{}`",
                    k.as_str().unwrap_or(crate::parser::token_kind_label(k)),
                ),
            );
            p.builder.start_node(SyntaxKind::ErrorNode, start);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
    }
}

fn parse_arg_list(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::ArgList, start);
    p.expect(TokenKind::LParen);
    while !p.at(TokenKind::RParen) && !p.at(TokenKind::Eof) {
        parse_expr(p);
        if !p.eat(TokenKind::Comma) {
            break;
        }
    }
    p.expect(TokenKind::RParen);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_if_expr(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::IfExpr, start);
    p.expect(TokenKind::KwIf);
    parse_cond_expr(p);
    if p.at(TokenKind::LBrace) {
        parse_block(p);
    } else {
        p.unexpected("`{` to begin the `then` block");
    }
    if p.eat(TokenKind::KwElse) {
        if p.at(TokenKind::KwIf) {
            parse_if_expr(p);
        } else if p.at(TokenKind::LBrace) {
            parse_block(p);
        } else {
            p.unexpected("`{` or `if` after `else`");
        }
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_while_expr(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::WhileExpr, start);
    p.expect(TokenKind::KwWhile);
    parse_cond_expr(p);
    if p.at(TokenKind::LBrace) {
        parse_block(p);
    } else {
        p.unexpected("`{` to begin the loop body");
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

/// Parse an expression in a position where the *trailing* `{` belongs
/// to the enclosing construct (the `if`/`while`/`for` body), not to a
/// struct literal continuation. Toggles
/// `parser.struct_literals_allowed` for the duration.
fn parse_cond_expr(p: &mut Parser<'_, '_, '_>) {
    let prev = p.struct_literals_allowed;
    p.struct_literals_allowed = false;
    parse_expr(p);
    p.struct_literals_allowed = prev;
}

// ─── match (Phase 2 increment M.1) ─────────────────────────────────────

fn parse_match_expr(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::MatchExpr, start);
    p.expect(TokenKind::KwMatch);
    // Same trick as parse_cond_expr: the scrutinee's trailing `{`
    // belongs to the match-arm-list, not a struct literal.
    let prev = p.struct_literals_allowed;
    p.struct_literals_allowed = false;
    parse_expr(p);
    p.struct_literals_allowed = prev;
    // Match arm list.
    let arm_list_start = p.cur_byte_start();
    p.builder
        .start_node(SyntaxKind::MatchArmList, arm_list_start);
    if p.expect(TokenKind::LBrace) {
        while !p.at(TokenKind::RBrace) && !p.at(TokenKind::Eof) {
            let before = p.pos;
            parse_match_arm(p);
            // Trailing comma is optional; arms without `,` end the list.
            if !p.eat(TokenKind::Comma) {
                break;
            }
            // Defensive: if `parse_match_arm` made no progress, bail out
            // so the parser can't loop forever on a malformed arm.
            if p.pos == before {
                break;
            }
        }
        p.expect(TokenKind::RBrace);
    }
    let arm_list_end = p.cur_byte_start();
    p.builder.finish_node(arm_list_end);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_match_arm(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::MatchArm, start);
    parse_match_pattern(p);
    p.expect(TokenKind::FatArrow);
    parse_expr(p);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

/// Pattern parser for match arms only. Differs from `parse_pattern`
/// (used by `let` and `for in`) by accepting `LiteralPat` shapes —
/// bare integer literals and `Unary(Minus, IntLit)`. Letting `let 5
/// = …` parse would silently widen the let surface, so the two
/// parsers are kept separate. Also handles top-level or-patterns
/// (`a | b | c`) and inclusive range patterns (`lo..=hi`); both land
/// in M.3.
fn parse_match_pattern(p: &mut Parser<'_, '_, '_>) {
    let cp = p.builder.checkpoint();
    let start = p.cur_byte_start();
    parse_match_pattern_atom(p);
    if p.at(TokenKind::Pipe) {
        // Wrap the first atom plus all subsequent `| atom` into a
        // single `OrPat`. The checkpoint captures the position
        // before the first atom so its node becomes the OrPat's
        // first child.
        p.builder.start_node_at(cp, SyntaxKind::OrPat, start);
        while p.eat(TokenKind::Pipe) {
            parse_match_pattern_atom(p);
        }
        let end = p.cur_byte_start();
        p.builder.finish_node(end);
    }
}

/// Parse a literal-only expression for a pattern slot: `IntLit`,
/// `-IntLit`, `true`, `false`. Avoids `parse_expr`'s Pratt operators
/// so `|` (bitwise OR, bp 9) and `..=` (range op) stay available for
/// the pattern grammar to consume.
fn parse_pattern_literal_value(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    if p.at(TokenKind::Minus) {
        p.builder.start_node(SyntaxKind::UnaryExpr, start);
        p.bump_any(); // -
        if p.at(TokenKind::IntLit) {
            let lit_start = p.cur_byte_start();
            p.builder.start_node(SyntaxKind::LiteralExpr, lit_start);
            p.bump_any();
            let lit_end = p.cur_byte_start();
            p.builder.finish_node(lit_end);
        } else {
            p.unexpected("integer literal after `-` in pattern");
        }
        let end = p.cur_byte_start();
        p.builder.finish_node(end);
    } else if matches!(
        p.current(),
        TokenKind::IntLit | TokenKind::KwTrue | TokenKind::KwFalse | TokenKind::KwNil
    ) {
        p.builder.start_node(SyntaxKind::LiteralExpr, start);
        p.bump_any();
        let end = p.cur_byte_start();
        p.builder.finish_node(end);
    } else {
        p.unexpected("integer literal, `true`, `false`, `nil`, or `-IntLit` in pattern");
    }
}

/// One pattern alternative: a single non-or-shaped pattern. Wildcards,
/// identifier patterns, literal patterns, and range patterns all land
/// here. Range detection happens after the initial literal expression
/// is parsed — if `..=` follows, we retroactively wrap the literal
/// into a `RangePat` and parse the upper bound.
fn parse_match_pattern_atom(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    match p.current() {
        TokenKind::Ident => {
            let span = p.current_span();
            let is_underscore = p.span_bytes(span) == b"_";
            let kind = if is_underscore {
                SyntaxKind::WildcardPat
            } else {
                SyntaxKind::IdentPat
            };
            p.builder.start_node(kind, start);
            p.bump_any();
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
        TokenKind::IntLit
        | TokenKind::Minus
        | TokenKind::KwTrue
        | TokenKind::KwFalse
        | TokenKind::KwNil => {
            // Parse the literal value as a *literal-only* expression
            // — `parse_expr` here would consume `|` as bitwise OR
            // (binding power 9), stealing the alternation token from
            // `parse_match_pattern`. Same reason the pattern parser
            // doesn't see `..=` ranges as range *expressions*.
            // After the literal, if `..=` follows, the whole atom is
            // a `RangePat` (the literal is its lower bound); else
            // wrap as `LiteralPat`.
            //
            // O.2: `nil` joins `KwTrue` / `KwFalse` as a literal-only
            // shape (the typeck accepts it only against `?T` scrutinees).
            let cp = p.builder.checkpoint();
            parse_pattern_literal_value(p);
            if p.at(TokenKind::DotDotEq) {
                p.builder.start_node_at(cp, SyntaxKind::RangePat, start);
                p.bump_any(); // consume `..=`
                parse_pattern_literal_value(p);
                let end = p.cur_byte_start();
                p.builder.finish_node(end);
            } else {
                p.builder.start_node_at(cp, SyntaxKind::LiteralPat, start);
                let end = p.cur_byte_start();
                p.builder.finish_node(end);
            }
        }
        _ => {
            let span = p.current_span();
            p.error(
                ec::EXPECTED_PATTERN,
                span,
                "expected a pattern (`_`, identifier, integer literal, `true`/`false`, or `lo..=hi`)",
            );
            p.builder.start_node(SyntaxKind::ErrorNode, start);
            let end = p.cur_byte_start();
            p.builder.finish_node(end);
        }
    }
}

fn parse_return_expr(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::ReturnExpr, start);
    p.expect(TokenKind::KwReturn);
    // Optional return value: present iff the next significant token is
    // not a stmt-boundary.
    if !p.at_any(&[TokenKind::Semi, TokenKind::RBrace, TokenKind::Eof]) {
        parse_expr(p);
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_break_expr(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::BreakExpr, start);
    p.expect(TokenKind::KwBreak);
    // Optional break value (Phase 1 doesn't actually thread it through,
    // but we accept the syntax).
    if !p.at_any(&[TokenKind::Semi, TokenKind::RBrace, TokenKind::Eof]) {
        parse_expr(p);
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_continue_expr(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::ContinueExpr, start);
    p.expect(TokenKind::KwContinue);
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}

fn parse_for_expr(p: &mut Parser<'_, '_, '_>) {
    let start = p.cur_byte_start();
    p.builder.start_node(SyntaxKind::ForExpr, start);
    p.expect(TokenKind::KwFor);
    parse_pattern(p);
    p.expect(TokenKind::KwIn);
    // Phase 1 only supports range iterators: `for x in EXPR..EXPR { ... }`
    // and `for x in EXPR..=EXPR { ... }`. Range expressions outside `for`
    // are a Phase 2+ feature; we parse them inline here so the `..`
    // tokens don't have to be added to `infix_bp`. Both bounds are
    // parsed in cond mode so a struct-literal `{` doesn't get glued
    // onto the upper bound's tail.
    parse_cond_expr(p);
    if !p.eat(TokenKind::DotDot) && !p.eat(TokenKind::DotDotEq) {
        p.unexpected("`..` or `..=` to begin a range");
    }
    parse_cond_expr(p);
    if p.at(TokenKind::LBrace) {
        parse_block(p);
    } else {
        p.unexpected("`{` to begin the loop body");
    }
    let end = p.cur_byte_start();
    p.builder.finish_node(end);
}
