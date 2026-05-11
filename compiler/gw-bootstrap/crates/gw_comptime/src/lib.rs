//! GW comptime engine — `comptime` blocks, `#run`, `#insert`, `@type_info`.
//!
//! Phase 2 implementation strategy is a **tree-walking interpreter on
//! the typed AST**, per `docs/architecture.md` Part B.11 + Part E.1.
//! Phase 5 replaces it with a stack VM operating on MIR; the on-disk
//! semantics (`CtValue`, sandbox budgets, error variants) carry over.
//!
//! CT.1 scope (this sub-bundle) — the smallest end-to-end comptime
//! tracer. The evaluator accepts a `comptime { … }` block whose tail
//! expression reduces to an integer literal (possibly wrapped in
//! parens or a unary minus). Statements inside the block, arithmetic,
//! and control flow are rejected with [`EvalError::Unsupported`]
//! until CT.2 lands the interpreter loop.

use gw_ast::ast::{AstNode, Block, Expr, LiteralExpr};
use gw_ast::SyntaxKind;
use gw_lex::{SourceMap, Span};

/// Result of evaluating one comptime expression. The width carried by
/// `Int` is left to the typed AST: typeck records the surrounding
/// expression's `Ty` in `expr_types`, and MIR uses that to pick the
/// `IntTy` for the materialised `Const::Int`.
#[derive(Copy, Clone, Debug)]
pub enum CtValue {
    /// Integer constant in two's-complement i128 representation.
    Int(i128),
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
}

impl EvalError {
    /// Primary span for diagnostic construction.
    pub fn primary_span(&self) -> Span {
        match self {
            Self::Unsupported { span, .. }
            | Self::BudgetExceeded(span)
            | Self::StackOverflow(span)
            | Self::BadIntLiteral(span) => *span,
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
/// CT.1 surface: the block must consist of zero statements and a
/// single tail expression that reduces to an integer literal
/// (possibly wrapped in parens or `Unary(Minus, …)`). Wider shapes
/// (let-bindings, control flow, arithmetic) are rejected with
/// [`EvalError::Unsupported`] until CT.2.
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
            match v? {
                CtValue::Int(n) => Ok(CtValue::Int(n.wrapping_neg())),
            }
        }
        Expr::Block(b) => {
            cx.enter(span)?;
            let v = eval_comptime_block_inner(b, span, cx);
            cx.exit();
            v
        }
        _ => Err(EvalError::Unsupported {
            span,
            what: "this expression shape is not yet supported by the CT.1 evaluator",
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
        _ => Err(EvalError::Unsupported {
            span,
            what: "literal kind not yet supported by the CT.1 evaluator",
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

    #[test]
    fn bare_int_literal() {
        let CtValue::Int(n) = eval_default("fn t() -> i32 { return comptime { 42 }; }").unwrap();
        assert_eq!(n, 42);
    }

    #[test]
    fn negated_literal() {
        let CtValue::Int(n) = eval_default("fn t() -> i32 { return comptime { -3 }; }").unwrap();
        assert_eq!(n, -3);
    }

    #[test]
    fn paren_wrapped() {
        let CtValue::Int(n) = eval_default("fn t() -> i32 { return comptime { (5) }; }").unwrap();
        assert_eq!(n, 5);
    }

    #[test]
    fn nested_block_tail() {
        // Bare `{ 7 }` at statement position is parsed as a block-like
        // statement (parse_stmt's LBrace arm), not as an expression.
        // Paren-wrapping forces the inner block into expression
        // context so it becomes the comptime block's tail expression
        // and exercises eval_expr's `Expr::Block` arm.
        let CtValue::Int(n) =
            eval_default("fn t() -> i32 { return comptime { ({ 7 }) }; }").unwrap();
        assert_eq!(n, 7);
    }

    #[test]
    fn hex_literal_decodes() {
        let CtValue::Int(n) = eval_default("fn t() -> i32 { return comptime { 0xff }; }").unwrap();
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

    #[test]
    fn arithmetic_is_unsupported_at_ct1() {
        let err = eval_default("fn t() -> i32 { return comptime { 1 + 2 }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported (binary ops land in CT.2), got {err:?}",
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
