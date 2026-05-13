//! GW comptime engine — `comptime` blocks, `#run`, `#insert`, `@type_info`.
//!
//! Phase 2 implementation strategy is a **tree-walking interpreter on
//! the typed AST**, per `docs/architecture.md` Part B.11 + Part E.1.
//! Phase 5 replaces it with a stack VM operating on MIR; the on-disk
//! semantics (`CtValue`, sandbox budgets, error variants) carry over.
//!
//! CT.1 scope — the smallest end-to-end comptime tracer. The
//! evaluator accepts a `comptime { … }` block whose tail expression
//! reduces to an integer literal (possibly wrapped in parens, a
//! unary minus, or another block).
//!
//! CT.2a — integer binary arithmetic. The evaluator gains an
//! `Expr::Binary` arm dispatching on `+ - * / %` over
//! `CtValue::Int(i128)` operands; overflow and division-by-zero
//! raise the new [`EvalError::IntegerOverflow`] /
//! [`EvalError::DivisionByZero`] variants.
//!
//! CT.2b (this sub-bundle) — comparisons + booleans. `CtValue` gains
//! the [`CtValue::Bool`] arm. `eval_binary` is reorganised around
//! op-first dispatch with three groups: integer arithmetic (CT.2a),
//! integer ordering (`< <= > >=`), and equality (`==` / `!=`,
//! overloaded for both integer and bool operands). Bool ordering
//! (`true < false`) and logical `&&` / `||` ride a later sub-bundle.
//! Operand-type checks go through [`expect_int`] so non-integer
//! operands of arithmetic / ordering ops produce a clear diagnostic
//! rather than a wrong answer. CT.2c will add `if`/`else` +
//! let-bindings + locals env.

use gw_ast::ast::{AstNode, BinaryExpr, Block, Expr, LiteralExpr};
use gw_ast::SyntaxKind;
use gw_lex::{SourceMap, Span};

/// Result of evaluating one comptime expression. The width carried by
/// `Int` is left to the typed AST: typeck records the surrounding
/// expression's `Ty` in `expr_types`, and MIR uses that to pick the
/// `IntTy` for the materialised `Const::Int`. `Bool` has no width
/// — MIR lowers it directly to `Const::Bool(b)`.
#[derive(Copy, Clone, Debug)]
pub enum CtValue {
    /// Integer constant in two's-complement i128 representation.
    Int(i128),
    /// Boolean constant. Added in CT.2b alongside the comparison ops.
    Bool(bool),
}

/// Error raised by the evaluator. Spans point into the original source
/// so the caller can build a diagnostic without re-deriving the span.
#[derive(Copy, Clone, Debug)]
pub enum EvalError {
    /// An expression or block shape the CT.1 evaluator does not yet
    /// handle. `what` is a short, span-independent description used
    /// inside the produced diagnostic.
    Unsupported {
        /// Span of the offending construct.
        span: Span,
        /// Short description of what was rejected.
        what: &'static str,
    },
    /// Step budget exhausted (architecture E.3 default: 10⁹ steps).
    BudgetExceeded(Span),
    /// Recursion depth exhausted (architecture E.3 default: 1024 frames).
    StackOverflow(Span),
    /// An integer literal could not be parsed as i128.
    BadIntLiteral(Span),
    /// A checked integer operation overflowed `i128`. CT.2's
    /// evaluator works in i128 arbitrary precision (decision Q1 in
    /// the HANDOFF's CT.2 plan); overflow here means the comptime
    /// computation itself exceeded i128, distinct from
    /// materialisation-time narrowing overflow that fires in
    /// `gw_typeck::lower_comptime` when the result doesn't fit the
    /// surrounding runtime type.
    IntegerOverflow(Span),
    /// `lhs / 0` or `lhs % 0` during evaluation. Span points at the
    /// offending binary expression.
    DivisionByZero(Span),
}

impl EvalError {
    /// Primary span for diagnostic construction.
    pub fn primary_span(&self) -> Span {
        match self {
            Self::Unsupported { span, .. }
            | Self::BudgetExceeded(span)
            | Self::StackOverflow(span)
            | Self::BadIntLiteral(span)
            | Self::IntegerOverflow(span)
            | Self::DivisionByZero(span) => *span,
        }
    }
}

/// Caps spec'd in `docs/architecture.md` E.3. CT.1's tracer only ever
/// takes a couple of steps, but wiring the budget through now means
/// CT.2's interpreter loop only has to call the existing step / enter
/// helpers.
#[derive(Copy, Clone, Debug)]
pub struct Budget {
    /// Maximum number of evaluator steps before [`EvalError::BudgetExceeded`].
    pub max_steps: u64,
    /// Maximum recursion depth before [`EvalError::StackOverflow`].
    pub max_depth: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_steps: 1_000_000_000,
            max_depth: 1024,
        }
    }
}

/// Mutable per-invocation state. A single `EvalCx` owns one comptime
/// invocation's step counter, recursion depth, and a borrow of the
/// source map (for slicing integer-literal text).
pub struct EvalCx<'sm> {
    sm: &'sm SourceMap,
    budget: Budget,
    steps: u64,
    depth: u32,
}

impl<'sm> EvalCx<'sm> {
    /// Fresh context with default budgets.
    pub fn new(sm: &'sm SourceMap) -> Self {
        Self {
            sm,
            budget: Budget::default(),
            steps: 0,
            depth: 0,
        }
    }

    /// Override the default budget. Used by tests; pipeline callers
    /// should accept the defaults.
    pub fn with_budget(sm: &'sm SourceMap, budget: Budget) -> Self {
        Self {
            sm,
            budget,
            steps: 0,
            depth: 0,
        }
    }

    fn step(&mut self, span: Span) -> Result<(), EvalError> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > self.budget.max_steps {
            return Err(EvalError::BudgetExceeded(span));
        }
        Ok(())
    }

    fn enter(&mut self, span: Span) -> Result<(), EvalError> {
        let next = self.depth.saturating_add(1);
        if next > self.budget.max_depth {
            return Err(EvalError::StackOverflow(span));
        }
        self.depth = next;
        Ok(())
    }

    fn exit(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

/// Evaluate the body of a `comptime { … }` block.
///
/// CT.1 + CT.2a + CT.2b surface: the block must consist of zero
/// statements and a single tail expression. The tail expression
/// may be an integer literal (CT.1), `true` / `false` (CT.2b), a
/// unary minus on an integer (CT.1), a parenthesised expression
/// (CT.1), a nested block (CT.1), a binary arithmetic expression
/// `+ - * / %` over integer operands (CT.2a), an integer ordering
/// comparison `< <= > >=` (CT.2b), or an `==` / `!=` over matching
/// integer-or-bool operands (CT.2b). let-bindings, control flow,
/// logical `&&` / `||`, and other shapes are rejected with
/// [`EvalError::Unsupported`] until CT.2c.
pub fn eval_comptime_block(block: Block<'_>, cx: &mut EvalCx<'_>) -> Result<CtValue, EvalError> {
    let span = block.syntax().span;
    cx.step(span)?;
    cx.enter(span)?;
    let result = eval_comptime_block_inner(block, span, cx);
    cx.exit();
    result
}

fn eval_comptime_block_inner(
    block: Block<'_>,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Result<CtValue, EvalError> {
    if block.stmts().next().is_some() {
        return Err(EvalError::Unsupported {
            span,
            what: "statements inside a `comptime` block are not yet supported (CT.2)",
        });
    }
    let Some(tail) = block.tail_expr() else {
        return Err(EvalError::Unsupported {
            span,
            what: "`comptime` block has no tail expression to evaluate",
        });
    };
    eval_expr(tail, cx)
}

fn eval_expr(expr: Expr<'_>, cx: &mut EvalCx<'_>) -> Result<CtValue, EvalError> {
    let span = expr.syntax().span;
    cx.step(span)?;
    match expr {
        Expr::Literal(l) => eval_literal(l, cx),
        Expr::Paren(p) => {
            let inner = p.inner().ok_or(EvalError::Unsupported {
                span,
                what: "empty parenthesised expression",
            })?;
            cx.enter(span)?;
            let v = eval_expr(inner, cx);
            cx.exit();
            v
        }
        Expr::Unary(u) if matches!(u.op_kind(), Some(SyntaxKind::Minus)) => {
            let operand = u.operand().ok_or(EvalError::Unsupported {
                span,
                what: "unary operator missing its operand",
            })?;
            cx.enter(span)?;
            let v = eval_expr(operand, cx);
            cx.exit();
            let n = expect_int(v?, span)?;
            Ok(CtValue::Int(n.wrapping_neg()))
        }
        Expr::Block(b) => {
            cx.enter(span)?;
            let v = eval_comptime_block_inner(b, span, cx);
            cx.exit();
            v
        }
        Expr::Binary(b) => {
            cx.enter(span)?;
            let v = eval_binary(b, span, cx);
            cx.exit();
            v
        }
        _ => Err(EvalError::Unsupported {
            span,
            what: "this expression shape is not yet supported by the CT.2 evaluator",
        }),
    }
}

/// Evaluate `lhs OP rhs`. CT.2a accepts arithmetic (`+`, `-`, `*`,
/// `/`, `%`) over `i128` operands; CT.2b adds the four integer
/// ordering comparisons (`<`, `<=`, `>`, `>=`) and overloaded
/// equality (`==`, `!=`) for both integer and boolean operands.
/// Bool ordering (`true < false`) and logical `&&` / `||` are
/// deferred. Arithmetic uses `i128` checked ops per decision Q1
/// (arbitrary-precision evaluator, narrow at materialisation): an
/// overflow during compile-time eval is a distinct error from a
/// materialisation-time narrowing overflow. Op-first dispatch
/// keeps each operator's operand-type contract local — arithmetic
/// and ordering route through [`expect_int`]; equality accepts
/// matching `(Int, Int)` or `(Bool, Bool)` pairs and rejects mixed
/// pairs explicitly so the user sees the type-mismatch rather than
/// an arbitrary dominant-type rule.
fn eval_binary(
    expr: BinaryExpr<'_>,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Result<CtValue, EvalError> {
    let lhs = expr.lhs().ok_or(EvalError::Unsupported {
        span,
        what: "binary operator missing its left operand",
    })?;
    let rhs = expr.rhs().ok_or(EvalError::Unsupported {
        span,
        what: "binary operator missing its right operand",
    })?;
    let op = expr.op_kind().ok_or(EvalError::Unsupported {
        span,
        what: "binary operator missing its operator token",
    })?;
    let lv = eval_expr(lhs, cx)?;
    let rv = eval_expr(rhs, cx)?;
    match op {
        SyntaxKind::Plus
        | SyntaxKind::Minus
        | SyntaxKind::Star
        | SyntaxKind::Slash
        | SyntaxKind::Percent => {
            let l = expect_int(lv, span)?;
            let r = expect_int(rv, span)?;
            let result = match op {
                SyntaxKind::Plus => l.checked_add(r).ok_or(EvalError::IntegerOverflow(span))?,
                SyntaxKind::Minus => l.checked_sub(r).ok_or(EvalError::IntegerOverflow(span))?,
                SyntaxKind::Star => l.checked_mul(r).ok_or(EvalError::IntegerOverflow(span))?,
                SyntaxKind::Slash => {
                    if r == 0 {
                        return Err(EvalError::DivisionByZero(span));
                    }
                    l.checked_div(r).ok_or(EvalError::IntegerOverflow(span))?
                }
                SyntaxKind::Percent => {
                    if r == 0 {
                        return Err(EvalError::DivisionByZero(span));
                    }
                    l.checked_rem(r).ok_or(EvalError::IntegerOverflow(span))?
                }
                _ => unreachable!("outer match guarantees arithmetic op"),
            };
            Ok(CtValue::Int(result))
        }
        SyntaxKind::Lt | SyntaxKind::LtEq | SyntaxKind::Gt | SyntaxKind::GtEq => {
            let l = expect_int(lv, span)?;
            let r = expect_int(rv, span)?;
            let result = match op {
                SyntaxKind::Lt => l < r,
                SyntaxKind::LtEq => l <= r,
                SyntaxKind::Gt => l > r,
                SyntaxKind::GtEq => l >= r,
                _ => unreachable!("outer match guarantees ordering op"),
            };
            Ok(CtValue::Bool(result))
        }
        SyntaxKind::EqEq | SyntaxKind::BangEq => {
            let equal = match (lv, rv) {
                (CtValue::Int(l), CtValue::Int(r)) => l == r,
                (CtValue::Bool(l), CtValue::Bool(r)) => l == r,
                _ => {
                    return Err(EvalError::Unsupported {
                        span,
                        what: "`==` / `!=` operands must have matching types in a comptime block",
                    });
                }
            };
            let result = if matches!(op, SyntaxKind::EqEq) {
                equal
            } else {
                !equal
            };
            Ok(CtValue::Bool(result))
        }
        _ => Err(EvalError::Unsupported {
            span,
            what: "this binary operator is not yet supported by the comptime evaluator",
        }),
    }
}

/// Pin a `CtValue` to its integer payload, or report an Unsupported
/// diagnostic at `span`. Used by every arithmetic and ordering op so
/// the operand-shape contract lives in exactly one place; CT.2c's
/// additions (let-bindings, branches) will route the same way.
fn expect_int(v: CtValue, span: Span) -> Result<i128, EvalError> {
    match v {
        CtValue::Int(n) => Ok(n),
        CtValue::Bool(_) => Err(EvalError::Unsupported {
            span,
            what: "this operator requires an integer operand, found `bool`",
        }),
    }
}

fn eval_literal(l: LiteralExpr<'_>, cx: &mut EvalCx<'_>) -> Result<CtValue, EvalError> {
    let (kind, span) = l.token().ok_or(EvalError::Unsupported {
        span: l.syntax().span,
        what: "literal expression without a token child",
    })?;
    match kind {
        SyntaxKind::IntLit => {
            let raw = cx.sm.slice(span).unwrap_or("");
            parse_int_literal(raw)
                .map(CtValue::Int)
                .ok_or(EvalError::BadIntLiteral(span))
        }
        SyntaxKind::KwTrue => Ok(CtValue::Bool(true)),
        SyntaxKind::KwFalse => Ok(CtValue::Bool(false)),
        _ => Err(EvalError::Unsupported {
            span,
            what: "this literal kind is not yet supported by the comptime evaluator",
        }),
    }
}

/// Parse an unsigned integer literal in its source form (`0xFF`,
/// `0b1010`, `0o17`, decimal with underscores) as i128. Mirrors
/// `gw_mir::parse_int_literal`; kept in lockstep so the CT.1
/// evaluator and the runtime lowering decode identical bytes.
fn parse_int_literal(raw: &str) -> Option<i128> {
    let s = raw.replace('_', "");
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i128::from_str_radix(rest, 16).ok()
    } else if let Some(rest) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        i128::from_str_radix(rest, 8).ok()
    } else if let Some(rest) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        i128::from_str_radix(rest, 2).ok()
    } else {
        s.parse::<i128>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bumpalo::Bump;
    use gw_ast::ast::ComptimeExpr;
    use gw_ast::cst::SyntaxNode;
    use gw_ast::FileArena;
    use gw_lex::SourceMap;
    use gw_parse::parse;

    #[test]
    fn parse_int_literal_handles_radix_prefixes() {
        assert_eq!(parse_int_literal("42"), Some(42));
        assert_eq!(parse_int_literal("0xff"), Some(255));
        assert_eq!(parse_int_literal("0b1010"), Some(10));
        assert_eq!(parse_int_literal("0o17"), Some(15));
        assert_eq!(parse_int_literal("1_000_000"), Some(1_000_000));
    }

    #[test]
    fn parse_int_literal_rejects_garbage() {
        assert_eq!(parse_int_literal("0xZZ"), None);
        assert_eq!(parse_int_literal("not-an-int"), None);
    }

    /// Find the first `ComptimeExpr` reachable from `node`, in
    /// pre-order, and return its inner `Block`.
    fn find_comptime_block<'a>(node: &'a SyntaxNode<'a>) -> Option<Block<'a>> {
        if let Some(c) = ComptimeExpr::cast(node) {
            return c.block();
        }
        for child in node.child_nodes() {
            if let Some(b) = find_comptime_block(child) {
                return Some(b);
            }
        }
        None
    }

    /// Parse `src`, locate the first comptime block, and evaluate it
    /// with the given budget. Returns `(SourceMap, result)` so callers
    /// can inspect spans embedded in errors if needed.
    fn eval(src: &str, budget: Budget) -> Result<CtValue, EvalError> {
        let mut sm = SourceMap::new();
        let file = sm.add_file("t.gw", src);
        let bytes = sm.get(file).unwrap().contents.as_bytes();
        let bump = Bump::new();
        let arena = FileArena::new(&bump, file);
        let (root, _diags) = parse(file, bytes, &arena);
        let block =
            find_comptime_block(root).expect("test source must contain a `comptime { ... }` block");
        let mut cx = EvalCx::with_budget(&sm, budget);
        eval_comptime_block(block, &mut cx)
    }

    fn eval_default(src: &str) -> Result<CtValue, EvalError> {
        eval(src, Budget::default())
    }

    fn assert_int(v: CtValue) -> i128 {
        match v {
            CtValue::Int(n) => n,
            other => panic!("expected CtValue::Int, got {other:?}"),
        }
    }

    fn assert_bool(v: CtValue) -> bool {
        match v {
            CtValue::Bool(b) => b,
            other => panic!("expected CtValue::Bool, got {other:?}"),
        }
    }

    #[test]
    fn bare_int_literal() {
        let n = assert_int(eval_default("fn t() -> i32 { return comptime { 42 }; }").unwrap());
        assert_eq!(n, 42);
    }

    #[test]
    fn negated_literal() {
        let n = assert_int(eval_default("fn t() -> i32 { return comptime { -3 }; }").unwrap());
        assert_eq!(n, -3);
    }

    #[test]
    fn paren_wrapped() {
        let n = assert_int(eval_default("fn t() -> i32 { return comptime { (5) }; }").unwrap());
        assert_eq!(n, 5);
    }

    #[test]
    fn nested_block_tail() {
        // Bare `{ 7 }` at statement position is parsed as a block-like
        // statement (parse_stmt's LBrace arm), not as an expression.
        // Paren-wrapping forces the inner block into expression
        // context so it becomes the comptime block's tail expression
        // and exercises eval_expr's `Expr::Block` arm.
        let n = assert_int(eval_default("fn t() -> i32 { return comptime { ({ 7 }) }; }").unwrap());
        assert_eq!(n, 7);
    }

    #[test]
    fn hex_literal_decodes() {
        let n = assert_int(eval_default("fn t() -> i32 { return comptime { 0xff }; }").unwrap());
        assert_eq!(n, 255);
    }

    #[test]
    fn statement_in_block_is_unsupported() {
        let err = eval_default("fn t() -> i32 { return comptime { let x = 1; x }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported, got {err:?}",
        );
    }

    // CT.2a: binary arithmetic on integer literals.

    #[test]
    fn binary_addition() {
        let n = assert_int(eval_default("fn t() -> i32 { return comptime { 1 + 2 }; }").unwrap());
        assert_eq!(n, 3);
    }

    #[test]
    fn binary_subtraction() {
        let n = assert_int(eval_default("fn t() -> i32 { return comptime { 10 - 3 }; }").unwrap());
        assert_eq!(n, 7);
    }

    #[test]
    fn binary_multiplication() {
        let n = assert_int(eval_default("fn t() -> i32 { return comptime { 6 * 7 }; }").unwrap());
        assert_eq!(n, 42);
    }

    #[test]
    fn binary_division_truncates() {
        let n = assert_int(eval_default("fn t() -> i32 { return comptime { 100 / 7 }; }").unwrap());
        assert_eq!(n, 14);
    }

    #[test]
    fn binary_modulo() {
        let n = assert_int(eval_default("fn t() -> i32 { return comptime { 100 % 7 }; }").unwrap());
        assert_eq!(n, 2);
    }

    #[test]
    fn binary_precedence_respects_pratt() {
        // `1 + 2 * 3` parses as `1 + (2 * 3)` via Pratt precedence;
        // the evaluator simply walks the resulting CST so it inherits
        // the correct precedence for free.
        let n =
            assert_int(eval_default("fn t() -> i32 { return comptime { 1 + 2 * 3 }; }").unwrap());
        assert_eq!(n, 7);
    }

    #[test]
    fn binary_negated_operand() {
        // Exercises the interaction between the Unary(Minus, ...)
        // arm (CT.1) and the Binary arm (CT.2a).
        let n = assert_int(eval_default("fn t() -> i32 { return comptime { -3 + 10 }; }").unwrap());
        assert_eq!(n, 7);
    }

    #[test]
    fn division_by_zero_rejected() {
        let err = eval_default("fn t() -> i32 { return comptime { 1 / 0 }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::DivisionByZero(_)),
            "expected DivisionByZero, got {err:?}",
        );
    }

    #[test]
    fn modulo_by_zero_rejected() {
        let err = eval_default("fn t() -> i32 { return comptime { 1 % 0 }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::DivisionByZero(_)),
            "expected DivisionByZero, got {err:?}",
        );
    }

    // CT.2b: booleans + comparisons.

    #[test]
    fn bool_true_literal() {
        let b = assert_bool(eval_default("fn t() -> bool { return comptime { true }; }").unwrap());
        assert!(b);
    }

    #[test]
    fn bool_false_literal() {
        let b = assert_bool(eval_default("fn t() -> bool { return comptime { false }; }").unwrap());
        assert!(!b);
    }

    #[test]
    fn integer_lt() {
        let b = assert_bool(eval_default("fn t() -> bool { return comptime { 1 < 2 }; }").unwrap());
        assert!(b);
    }

    #[test]
    fn integer_le_is_inclusive() {
        let b =
            assert_bool(eval_default("fn t() -> bool { return comptime { 2 <= 2 }; }").unwrap());
        assert!(b);
    }

    #[test]
    fn integer_gt() {
        let b = assert_bool(eval_default("fn t() -> bool { return comptime { 3 > 2 }; }").unwrap());
        assert!(b);
    }

    #[test]
    fn integer_ge_at_boundary() {
        let b =
            assert_bool(eval_default("fn t() -> bool { return comptime { 2 >= 2 }; }").unwrap());
        assert!(b);
    }

    #[test]
    fn integer_eq_ne() {
        let eq_true =
            assert_bool(eval_default("fn t() -> bool { return comptime { 5 == 5 }; }").unwrap());
        assert!(eq_true);
        let ne_true =
            assert_bool(eval_default("fn t() -> bool { return comptime { 5 != 6 }; }").unwrap());
        assert!(ne_true);
    }

    #[test]
    fn bool_eq_ne() {
        let eq_true = assert_bool(
            eval_default("fn t() -> bool { return comptime { true == true }; }").unwrap(),
        );
        assert!(eq_true);
        let ne_true = assert_bool(
            eval_default("fn t() -> bool { return comptime { true != false }; }").unwrap(),
        );
        assert!(ne_true);
    }

    #[test]
    fn negated_operand_with_comparison() {
        // -3 < 0 — exercises the Unary(Minus) + Lt interaction (the
        // negated literal flows through the integer-ordering arm
        // without special handling).
        let b =
            assert_bool(eval_default("fn t() -> bool { return comptime { -3 < 0 }; }").unwrap());
        assert!(b);
    }

    #[test]
    fn arithmetic_on_bool_rejects() {
        // `1 + true` — expect_int should fire on the rhs operand and
        // produce a clear Unsupported diagnostic. The typeck would
        // normally reject this before we reach the evaluator, but
        // the evaluator's contract is independent of typeck so we
        // exercise it directly.
        let err = eval_default("fn t() -> i32 { return comptime { 1 + true }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported, got {err:?}",
        );
    }

    #[test]
    fn ordering_on_bool_rejects() {
        // `true < false` — bool ordering is deliberately deferred.
        let err = eval_default("fn t() -> i32 { return comptime { true < false }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported, got {err:?}",
        );
    }

    #[test]
    fn mixed_eq_rejects() {
        // `1 == true` has no obvious comparison semantics — reject
        // explicitly rather than inventing a dominant-type rule.
        let err = eval_default("fn t() -> bool { return comptime { 1 == true }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported, got {err:?}",
        );
    }

    #[test]
    fn logical_and_still_rejects() {
        // `&&` / `||` are deferred — make sure the evaluator's
        // catch-all `_` arm catches them with a clear Unsupported
        // rather than producing a wrong answer.
        let err =
            eval_default("fn t() -> bool { return comptime { true && false }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported (logical && is deferred), got {err:?}",
        );
    }

    #[test]
    fn step_budget_exhaustion() {
        // The block consumes 1 step; evaluating the tail expression
        // would need a 2nd step. A 1-step budget therefore trips
        // `BudgetExceeded` before the literal is read.
        let err = eval(
            "fn t() -> i32 { return comptime { 1 }; }",
            Budget {
                max_steps: 1,
                max_depth: 1024,
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, EvalError::BudgetExceeded(_)),
            "expected BudgetExceeded, got {err:?}",
        );
    }

    #[test]
    fn recursion_depth_exhaustion() {
        // The block enters once (depth 1); the paren-wrapping recurse
        // would push depth to 2, which a `max_depth: 1` budget rejects.
        let err = eval(
            "fn t() -> i32 { return comptime { (1) }; }",
            Budget {
                max_steps: 1_000_000_000,
                max_depth: 1,
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, EvalError::StackOverflow(_)),
            "expected StackOverflow, got {err:?}",
        );
    }
}
