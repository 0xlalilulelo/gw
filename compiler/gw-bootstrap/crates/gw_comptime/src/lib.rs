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
//! CT.2b — comparisons + booleans. `CtValue` gains the
//! [`CtValue::Bool`] arm. `eval_binary` is reorganised around
//! op-first dispatch with three groups: arithmetic, ordering
//! (`< <= > >=`), and equality (`==` / `!=`). Bool ordering
//! (`true < false`) and logical `&&` / `||` ride a later sub-bundle.
//! As of CT.3a, the three op groups dispatch on the operand-value
//! tuple rather than routing through a canonical `expect_int`
//! helper — both `(Int, Int)` and `(Float, Float)` pairs are
//! accepted, while mixed and Bool-in-arithmetic pairs reject
//! explicitly so the user sees the type mismatch directly.
//!
//! CT.2c — let-bindings + locals env. The evaluator gains a
//! [`BindingEnv`] trait that abstracts CST-node → binding-index
//! lookup so `gw_comptime` doesn't need to depend on `gw_typeck`
//! (which would form a cycle). [`EvalCx`] carries a
//! `Vec<Option<CtValue>>` indexed by `BindingId.0` (decision Q5 ⇒
//! option (a) — dense vector, mirrors runtime MIR's `Local`
//! indexing). `eval_comptime_block_inner` walks statements:
//! `Stmt::Let` evaluates the initialiser and stores it in `locals`
//! at the index supplied by the binding env. A new `Expr::Path`
//! arm in `eval_expr` reads the same env to materialise a local
//! reference.
//!
//! CT.2d — `if`/`else` control flow. The evaluator gains an
//! `Expr::If` arm: the condition is evaluated via [`expect_bool`]
//! (analog of [`expect_int`]) and exactly one arm is evaluated.
//! The un-taken arm is never visited, so any side effect (a
//! let-init that would panic, a comparison that would otherwise
//! produce `Unsupported`, a `1 / 0`) stays latent. This is the
//! first comptime sub-bundle where the evaluator's control flow
//! diverges from the syntactic shape the typed AST exposes.
//! Else-if chains fall out naturally because `IfExpr::else_branch`
//! returns an `Expr` — either a `Block` (terminal `else { … }`)
//! or another `IfExpr` (chained `else if`) — and both shapes are
//! already dispatched by `eval_expr`.
//!
//! CT.2e — short-circuit `&&` / `||`. Pure recombination of
//! CT.2b's `CtValue::Bool` + CT.2d's branch-eval discipline
//! applied at the operator level rather than the statement level.
//! `eval_binary` intercepts `SyntaxKind::AmpAmp` / `PipePipe`
//! *before* its eager RHS eval and dispatches to
//! [`eval_logical_short_circuit`], which evaluates the LHS, pins
//! it to bool via [`expect_bool`], and evaluates the RHS only
//! when the LHS doesn't determine the result (`false` for `&&`,
//! `true` for `||`). The RHS therefore never runs when it would
//! short-circuit — `false && (1 / 0 == 0)` returns false rather
//! than raising `DivisionByZero`.
//!
//! CT.3b (this sub-bundle) — string literals. `CtValue` gains
//! `Str(Vec<u8>)` (owned-inline; `CtValue` loses its `Copy`
//! impl here because `Vec<u8>` isn't `Copy`). `eval_literal`
//! recognises `SyntaxKind::StringLit` and decodes via a new
//! [`decode_string_literal`] helper kept in lockstep with
//! `gw_mir::decode_string_literal`. No comptime operations on
//! strings (concat, `==`, `.len`) yet — they ride a future
//! sub-bundle motivated by corpus need. Materialisation at MIR
//! time produces the same `{data, len}` `[]u8` slice aggregate
//! that `lower_string_literal` builds for runtime literals, so
//! the rodata path is shared between the two.

use gw_ast::ast::{AstNode, BinaryExpr, Block, Expr, IfExpr, LetStmt, LiteralExpr, Pattern, Stmt};
use gw_ast::cst::NodePtr;
use gw_ast::SyntaxKind;
use gw_lex::{SourceMap, Span};

/// Result of evaluating one comptime expression. The width carried by
/// `Int` is left to the typed AST: typeck records the surrounding
/// expression's `Ty` in `expr_types`, and MIR uses that to pick the
/// `IntTy` for the materialised `Const::Int`. `Bool` has no width
/// — MIR lowers it directly to `Const::Bool(b)`. `Float` is canonical
/// at `f64`; MIR narrows to `f32` at materialisation time if the
/// surrounding expression's type is `Ty::Float(F32)` (same pattern as
/// the runtime `FloatLit` lowering in `gw_mir::lower_literal`).
/// `Str` carries the decoded bytes of a string literal — owned
/// inline, with no `Copy` because `Vec<u8>` isn't `Copy`. Materialises
/// at MIR time as a `[]u8` slice aggregate: bytes get interned into
/// `MirProgram::string_literals`, then a fresh aggregate local holds
/// `{data: Const::DataAddr(id), len: bytes.len() as USize}`.
#[derive(Clone, Debug)]
pub enum CtValue {
    /// Integer constant in two's-complement i128 representation.
    Int(i128),
    /// Boolean constant. Added in CT.2b alongside the comparison ops.
    Bool(bool),
    /// IEEE-754 floating-point constant. Added in CT.3a alongside
    /// `FloatLit` recognition and the float arithmetic / ordering
    /// dispatch in [`eval_binary`].
    Float(f64),
    /// String literal bytes (decoded; no NUL terminator). Added in
    /// CT.3b; `CtValue` lost its `Copy` impl here because owning the
    /// payload inline is simpler than threading a `&mut` storage
    /// borrow through every `EvalCx` consumer. Comptime operations
    /// on strings (concat, `==`, `.len`) ride a future sub-bundle —
    /// CT.3b only handles literal evaluation + slice materialisation.
    Str(Vec<u8>),
}

/// Resolver from CST node → binding index. Implemented by typeck via a
/// thin adapter over `TypedModule`'s `pat_bindings` / `path_bindings`
/// maps. The evaluator only needs the numeric `u32` index (== the
/// `pub` field of typeck's `BindingId`), keeping `gw_comptime`
/// independent of `gw_typeck`'s `BindingId` newtype so there is no
/// dep cycle (typeck calls into `gw_comptime`, not the other way
/// round).
pub trait BindingEnv<'a> {
    /// The local index assigned to a `let` pattern (`IdentPat`).
    /// Returns `None` if the pattern was never registered (e.g.
    /// typeck rejected this `let` before allocating a binding).
    fn lookup_pat(&self, node: NodePtr<'a>) -> Option<u32>;
    /// The local index a path expression resolved to. Returns
    /// `None` if the path doesn't resolve to a local (top-level
    /// fn, unresolved name, …).
    fn lookup_path(&self, node: NodePtr<'a>) -> Option<u32>;
}

/// Empty binding env that resolves nothing. Use this for evaluating
/// CT.1 / CT.2a / CT.2b shapes (no `let`, no path refs to locals)
/// and for unit tests that don't need typeck's resolver.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoBindings;

impl<'a> BindingEnv<'a> for NoBindings {
    fn lookup_pat(&self, _: NodePtr<'a>) -> Option<u32> {
        None
    }
    fn lookup_path(&self, _: NodePtr<'a>) -> Option<u32> {
        None
    }
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
    /// A float literal could not be parsed as `f64`. Added in CT.3a.
    BadFloatLiteral(Span),
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
            | Self::BadFloatLiteral(span)
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
/// invocation's step counter, recursion depth, a borrow of the source
/// map (for slicing integer-literal text), a borrow of the binding
/// env (for resolving `let`-patterns and path expressions), and a
/// dense `Vec` of `let`-bound local values indexed by binding index.
///
/// The `'sm` lifetime ties the cx to the source map; `'env` ties it
/// to the binding resolver; `'a` ties it to the bump-allocated CST.
pub struct EvalCx<'sm, 'env, 'a> {
    sm: &'sm SourceMap,
    bindings: &'env dyn BindingEnv<'a>,
    /// Dense locals env indexed by `BindingId.0 as usize`. `None`
    /// for indices not yet assigned by a `let`; the evaluator
    /// treats a `Some(_)` read after the binding's `let` has run
    /// as the only legal path. Reading `None` raises Unsupported
    /// (defensive — typeck's name-resolution should make this
    /// unreachable for well-typed programs).
    locals: Vec<Option<CtValue>>,
    budget: Budget,
    steps: u64,
    depth: u32,
}

impl<'sm, 'env, 'a> EvalCx<'sm, 'env, 'a> {
    /// Fresh context with default budgets. Pass `&NoBindings` when
    /// evaluating shapes that have no `let` / path-to-local
    /// references (CT.1, CT.2a, CT.2b corpus).
    pub fn new(sm: &'sm SourceMap, bindings: &'env dyn BindingEnv<'a>) -> Self {
        Self::with_budget(sm, bindings, Budget::default())
    }

    /// Override the default budget. Used by tests; pipeline callers
    /// should accept the defaults.
    pub fn with_budget(
        sm: &'sm SourceMap,
        bindings: &'env dyn BindingEnv<'a>,
        budget: Budget,
    ) -> Self {
        Self {
            sm,
            bindings,
            locals: Vec::new(),
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

    /// Store a value at local index `idx`, growing the env as needed
    /// so the binding-index → local-slot mapping is dense from
    /// position 0. CT.2c only ever appends in BindingId order
    /// (typeck allocates sequentially), but the resize lets a
    /// future shadow / branch interleaving land without an extra
    /// invariant.
    fn store_local(&mut self, idx: u32, value: CtValue) {
        let i = idx as usize;
        if self.locals.len() <= i {
            self.locals.resize(i + 1, None);
        }
        self.locals[i] = Some(value);
    }

    /// Read a local at `idx`. Returns `EvalError::Unsupported` at
    /// `span` if the slot was never assigned — defensive, since
    /// typeck's name-resolution should make a use-before-`let`
    /// unreachable from well-typed programs.
    fn load_local(&self, idx: u32, span: Span) -> Result<CtValue, EvalError> {
        let i = idx as usize;
        self.locals
            .get(i)
            .and_then(|slot| slot.as_ref())
            .cloned()
            .ok_or(EvalError::Unsupported {
                span,
                what: "comptime read of an uninitialised local",
            })
    }
}

/// Evaluate the body of a `comptime { … }` block.
///
/// CT.1 + CT.2a + CT.2b + CT.2c + CT.2d + CT.2e surface: the block
/// may contain zero or more `let` statements followed by a single
/// tail expression. `let` patterns are limited to `IdentPat`
/// (single name); the initialiser must be a supported expression.
/// The tail expression may be an integer literal (CT.1), `true` /
/// `false` (CT.2b), a unary minus on an integer (CT.1), a
/// parenthesised expression (CT.1), a nested block (CT.1), a path
/// reference to a previously `let`-bound local (CT.2c), a binary
/// arithmetic expression `+ - * / %` over integer operands
/// (CT.2a), an integer ordering comparison `< <= > >=` (CT.2b), an
/// `==` / `!=` over matching integer-or-bool operands (CT.2b), an
/// `if cond { … } else { … }` (CT.2d — including `else if` chains)
/// where the condition evaluates to a `CtValue::Bool`, or a
/// short-circuit `&&` / `||` over bool operands (CT.2e). Expression
/// statements and other shapes are rejected with
/// [`EvalError::Unsupported`].
pub fn eval_comptime_block<'a>(
    block: Block<'a>,
    cx: &mut EvalCx<'_, '_, 'a>,
) -> Result<CtValue, EvalError> {
    let span = block.syntax().span;
    cx.step(span)?;
    cx.enter(span)?;
    let result = eval_comptime_block_inner(block, span, cx);
    cx.exit();
    result
}

fn eval_comptime_block_inner<'a>(
    block: Block<'a>,
    span: Span,
    cx: &mut EvalCx<'_, '_, 'a>,
) -> Result<CtValue, EvalError> {
    for stmt in block.stmts() {
        cx.step(span)?;
        match stmt {
            Stmt::Let(l) => eval_let(l, cx)?,
            Stmt::Expr(e) => {
                return Err(EvalError::Unsupported {
                    span: e.syntax().span,
                    what: "expression statements inside a `comptime` block are not yet supported (CT.2d will add `if`/`else`)",
                });
            }
            Stmt::Stub(s) => {
                return Err(EvalError::Unsupported {
                    span: s.span,
                    what: "this statement kind is not supported in a `comptime` block",
                });
            }
            Stmt::Error(s) => {
                return Err(EvalError::Unsupported {
                    span: s.span,
                    what: "parse error inside `comptime` block",
                });
            }
        }
    }
    let Some(tail) = block.tail_expr() else {
        return Err(EvalError::Unsupported {
            span,
            what: "`comptime` block has no tail expression to evaluate",
        });
    };
    eval_expr(tail, cx)
}

/// Evaluate a `let pat = init;` statement. CT.2c accepts only
/// `IdentPat` patterns; wildcards and structural patterns reject
/// with [`EvalError::Unsupported`]. The initialiser must be a
/// supported expression shape. On success, the value lands in
/// `cx.locals[bid as usize]` where `bid` is the typeck-assigned
/// binding index.
fn eval_let<'a>(l: LetStmt<'a>, cx: &mut EvalCx<'_, '_, 'a>) -> Result<(), EvalError> {
    let span = l.syntax().span;
    let Some(init) = l.init() else {
        return Err(EvalError::Unsupported {
            span,
            what: "`let` without an initialiser is not supported in a `comptime` block",
        });
    };
    let value = eval_expr(init, cx)?;
    let Some(Pattern::Ident(p)) = l.pattern() else {
        return Err(EvalError::Unsupported {
            span,
            what: "only simple `let <name>` patterns are supported in a `comptime` block (CT.2c)",
        });
    };
    let Some(idx) = cx.bindings.lookup_pat(NodePtr(p.syntax())) else {
        return Err(EvalError::Unsupported {
            span,
            what: "comptime `let`-binding could not be resolved to a local index",
        });
    };
    cx.store_local(idx, value);
    Ok(())
}

fn eval_expr<'a>(expr: Expr<'a>, cx: &mut EvalCx<'_, '_, 'a>) -> Result<CtValue, EvalError> {
    let span = expr.syntax().span;
    cx.step(span)?;
    match expr {
        Expr::Literal(l) => eval_literal(l, cx),
        Expr::Path(p) => {
            let Some(idx) = cx.bindings.lookup_path(NodePtr(p.syntax())) else {
                return Err(EvalError::Unsupported {
                    span,
                    what: "path expression in a `comptime` block did not resolve to a local",
                });
            };
            cx.load_local(idx, span)
        }
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
                CtValue::Float(f) => Ok(CtValue::Float(-f)),
                CtValue::Bool(_) => Err(EvalError::Unsupported {
                    span,
                    what: "unary `-` requires a numeric operand, found `bool`",
                }),
                CtValue::Str(_) => Err(EvalError::Unsupported {
                    span,
                    what: "unary `-` requires a numeric operand, found `string`",
                }),
            }
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
        Expr::If(i) => {
            cx.enter(span)?;
            let v = eval_if(i, span, cx);
            cx.exit();
            v
        }
        _ => Err(EvalError::Unsupported {
            span,
            what: "this expression shape is not yet supported by the comptime evaluator",
        }),
    }
}

/// Evaluate an `if cond { then } else { else_arm }`. The condition
/// is evaluated and constrained to `CtValue::Bool` via
/// [`expect_bool`]; exactly one arm runs depending on the result.
/// The un-taken arm is never visited — `lower_pattern_test`-style
/// branch-eval discipline, decoupled from typeck's syntactic
/// walk-both-arms type check.
///
/// `else if` chains fall out naturally: `IfExpr::else_branch` returns
/// an `Expr`, which is dispatched through the standard `eval_expr`
/// match. A terminal `else { … }` arrives as `Expr::Block`; a
/// chained `else if` arrives as `Expr::If` and recurses through
/// this same function. An `if` without `else` used as a
/// value-producing expression is a typeck-side error (the if's
/// type would be `Ty::U0`, and CT.2c's inner-type gate already
/// rejects it before the evaluator runs); the defensive arm
/// below treats it as `Unsupported`.
fn eval_if<'a>(
    i: IfExpr<'a>,
    span: Span,
    cx: &mut EvalCx<'_, '_, 'a>,
) -> Result<CtValue, EvalError> {
    let cond = i.cond().ok_or(EvalError::Unsupported {
        span,
        what: "`if` is missing its condition expression",
    })?;
    let cond_v = eval_expr(cond, cx)?;
    let cond_b = expect_bool(cond_v, span)?;
    if cond_b {
        let then_block = i.then_block().ok_or(EvalError::Unsupported {
            span,
            what: "`if` is missing its then-block",
        })?;
        eval_comptime_block_inner(then_block, span, cx)
    } else if let Some(else_branch) = i.else_branch() {
        eval_expr(else_branch, cx)
    } else {
        Err(EvalError::Unsupported {
            span,
            what: "`if` without `else` inside a `comptime` block cannot produce a value",
        })
    }
}

/// Evaluate `lhs OP rhs`. CT.2a accepts arithmetic (`+`, `-`, `*`,
/// `/`, `%`) over `i128` operands; CT.2b adds the four integer
/// ordering comparisons (`<`, `<=`, `>`, `>=`) and overloaded
/// equality (`==`, `!=`) for both integer and boolean operands.
/// **CT.3a** widens arithmetic, ordering, and equality to admit
/// `(Float, Float)` operand pairs alongside `(Int, Int)`. Mixed
/// `(Int, Float)` / `(Float, Int)` pairs reject explicitly so the
/// user sees the type mismatch rather than an arbitrary
/// dominant-type rule (matches the runtime requirement of an
/// explicit `as` cast). Bool ordering (`true < false`) and logical
/// `&&` / `||` are still deferred. Integer arithmetic uses `i128`
/// checked ops per decision Q1; float arithmetic uses Rust's
/// IEEE-754 ops directly — `+ - * /` are total, division by `0.0`
/// yields `±∞` / `NaN`, `%` is `f64::rem`. Float ordering / equality
/// use Rust's `<` / `<=` / `>` / `>=` / `==` which already implement
/// the IEEE-754 partial-order contract (any comparison involving
/// `NaN` returns `false`, including `NaN == NaN`).
fn eval_binary<'a>(
    expr: BinaryExpr<'a>,
    span: Span,
    cx: &mut EvalCx<'_, '_, 'a>,
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
    // CT.2e: short-circuit operators evaluate LHS first, RHS only
    // when LHS doesn't determine the result. Must intercept
    // *before* the eager RHS eval below — otherwise the RHS would
    // always run, defeating the short-circuit semantics that match
    // the runtime `&&` / `||` lowering (decision #15).
    if matches!(op, SyntaxKind::AmpAmp | SyntaxKind::PipePipe) {
        return eval_logical_short_circuit(op, lhs, rhs, span, cx);
    }
    let lv = eval_expr(lhs, cx)?;
    let rv = eval_expr(rhs, cx)?;
    match op {
        SyntaxKind::Plus
        | SyntaxKind::Minus
        | SyntaxKind::Star
        | SyntaxKind::Slash
        | SyntaxKind::Percent => match (lv, rv) {
            (CtValue::Int(l), CtValue::Int(r)) => {
                let result = match op {
                    SyntaxKind::Plus => l.checked_add(r).ok_or(EvalError::IntegerOverflow(span))?,
                    SyntaxKind::Minus => {
                        l.checked_sub(r).ok_or(EvalError::IntegerOverflow(span))?
                    }
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
            // CT.3a: IEEE-754 arithmetic over `f64`. `+ - * / %` are
            // total operations — `Slash` / `Percent` by `0.0` yield
            // `±∞` / `NaN` per IEEE-754, no `DivisionByZero` error
            // (matches runtime semantics; the integer arms above are
            // the only path that traps on divide-by-zero).
            (CtValue::Float(l), CtValue::Float(r)) => {
                let result = match op {
                    SyntaxKind::Plus => l + r,
                    SyntaxKind::Minus => l - r,
                    SyntaxKind::Star => l * r,
                    SyntaxKind::Slash => l / r,
                    SyntaxKind::Percent => l % r,
                    _ => unreachable!("outer match guarantees arithmetic op"),
                };
                Ok(CtValue::Float(result))
            }
            _ => Err(EvalError::Unsupported {
                span,
                what: "arithmetic operands must both be int or both be float in a comptime block",
            }),
        },
        SyntaxKind::Lt | SyntaxKind::LtEq | SyntaxKind::Gt | SyntaxKind::GtEq => {
            // Float ordering: Rust's `<`, `<=`, `>`, `>=` on f64
            // return false for any operand pair involving NaN, which
            // is the IEEE-754 partial-order contract we want.
            let result = match (lv, rv) {
                (CtValue::Int(l), CtValue::Int(r)) => match op {
                    SyntaxKind::Lt => l < r,
                    SyntaxKind::LtEq => l <= r,
                    SyntaxKind::Gt => l > r,
                    SyntaxKind::GtEq => l >= r,
                    _ => unreachable!("outer match guarantees ordering op"),
                },
                (CtValue::Float(l), CtValue::Float(r)) => match op {
                    SyntaxKind::Lt => l < r,
                    SyntaxKind::LtEq => l <= r,
                    SyntaxKind::Gt => l > r,
                    SyntaxKind::GtEq => l >= r,
                    _ => unreachable!("outer match guarantees ordering op"),
                },
                _ => {
                    return Err(EvalError::Unsupported {
                        span,
                        what: "ordering operands must both be int or both be float in a comptime block",
                    });
                }
            };
            Ok(CtValue::Bool(result))
        }
        SyntaxKind::EqEq | SyntaxKind::BangEq => {
            // Float equality: `NaN == NaN` is false per IEEE-754
            // (Rust's `==` on f64 implements this directly).
            let equal = match (lv, rv) {
                (CtValue::Int(l), CtValue::Int(r)) => l == r,
                (CtValue::Bool(l), CtValue::Bool(r)) => l == r,
                (CtValue::Float(l), CtValue::Float(r)) => l == r,
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

/// Evaluate a short-circuit `&&` / `||`. Mirrors the runtime
/// lowering (decision #15): the LHS is evaluated first; the RHS
/// runs only when the LHS doesn't determine the result. `&&`
/// short-circuits on `false`; `||` short-circuits on `true`. The
/// "short-circuit value" is the LHS value that immediately fixes
/// the result without consulting the RHS — namely the operator's
/// identity element under boolean conjunction / disjunction
/// (`false` for AND, `true` for OR). When the LHS doesn't
/// short-circuit, the result is the RHS pinned to bool — both `true
/// && rhs` and `false || rhs` yield exactly `rhs`'s bool value.
fn eval_logical_short_circuit<'a>(
    op: SyntaxKind,
    lhs: Expr<'a>,
    rhs: Expr<'a>,
    span: Span,
    cx: &mut EvalCx<'_, '_, 'a>,
) -> Result<CtValue, EvalError> {
    let lv = eval_expr(lhs, cx)?;
    let l = expect_bool(lv, span)?;
    // `&&` short-circuits on false; `||` on true.
    let short_circuit_value = matches!(op, SyntaxKind::PipePipe);
    if l == short_circuit_value {
        return Ok(CtValue::Bool(short_circuit_value));
    }
    let rv = eval_expr(rhs, cx)?;
    let r = expect_bool(rv, span)?;
    Ok(CtValue::Bool(r))
}

/// Pin a `CtValue` to its boolean payload, or report an Unsupported
/// diagnostic at `span`. Used by `if`-condition evaluation and (in
/// CT.2e) by logical `&&` / `||` operand checks. The binary
/// arithmetic / ordering arms in [`eval_binary`] dispatch on the
/// operand tuple directly (since CT.3a accepts both `(Int, Int)`
/// and `(Float, Float)` pairs) rather than routing through a
/// canonical `expect_int` helper.
fn expect_bool(v: CtValue, span: Span) -> Result<bool, EvalError> {
    match v {
        CtValue::Bool(b) => Ok(b),
        CtValue::Int(_) => Err(EvalError::Unsupported {
            span,
            what: "this operator requires a bool operand, found `int`",
        }),
        CtValue::Float(_) => Err(EvalError::Unsupported {
            span,
            what: "this operator requires a bool operand, found `float`",
        }),
        CtValue::Str(_) => Err(EvalError::Unsupported {
            span,
            what: "this operator requires a bool operand, found `string`",
        }),
    }
}

fn eval_literal<'a>(l: LiteralExpr<'a>, cx: &mut EvalCx<'_, '_, 'a>) -> Result<CtValue, EvalError> {
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
        SyntaxKind::FloatLit => {
            let raw = cx.sm.slice(span).unwrap_or("");
            parse_float_literal(raw)
                .map(CtValue::Float)
                .ok_or(EvalError::BadFloatLiteral(span))
        }
        SyntaxKind::StringLit => {
            let raw = cx.sm.slice(span).unwrap_or("");
            Ok(CtValue::Str(decode_string_literal(raw)))
        }
        // `RawStringLit` (`\\…\\` GW syntax) deliberately not handled
        // here — the runtime `gw_mir::decode_string_literal` would
        // mis-decode the `\\` delimiter as the `\\` escape sequence,
        // a latent bug that no corpus program exercises. CT.3b stays
        // in lockstep with the validated runtime path; a corpus
        // motivation for raw strings would fix the decoder in both
        // places in lockstep.
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

/// Parse a float literal in its source form as `f64`. Mirrors the
/// runtime path in `gw_mir::lower_literal`'s `FloatLit` arm
/// (`raw.replace('_', "").parse::<f64>()`), kept in lockstep so the
/// comptime evaluator and the runtime lowering decode identical bit
/// patterns. Materialisation-time narrowing to `f32` lives in
/// `gw_mir::lower_comptime`, not here.
fn parse_float_literal(raw: &str) -> Option<f64> {
    raw.replace('_', "").parse::<f64>().ok()
}

/// Decode a `"..."` string literal token into its raw bytes. Mirrors
/// `gw_mir::decode_string_literal` exactly so the comptime evaluator
/// and the runtime lowering produce identical bytes for the same
/// source. Strips the surrounding double quotes the lexer leaves on
/// the token text and processes the small set of Phase-1-supported
/// escape sequences: `\n`, `\t`, `\r`, `\0`, `\\`, `\"`, `\'`.
/// Unknown escapes pass through literally (the leading backslash +
/// the following byte). Raw-string (`\\…\\`) tokens go through a
/// different lex shape and are deliberately not handled here — see
/// the `eval_literal` arm comment for why.
fn decode_string_literal(raw: &str) -> Vec<u8> {
    let inner = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    let mut out = Vec::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('r') => out.push(b'\r'),
            Some('0') => out.push(0),
            Some('\\') => out.push(b'\\'),
            Some('"') => out.push(b'"'),
            Some('\'') => out.push(b'\''),
            Some(other) => {
                out.push(b'\\');
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
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
    /// with the given budget. Uses a `NoBindings` env — so any test
    /// source that exercises `let` or path-to-local would reject as
    /// Unsupported; the dedicated let-binding tests below use the
    /// dual-walk helper [`eval_with_bindings`] instead.
    fn eval(src: &str, budget: Budget) -> Result<CtValue, EvalError> {
        let mut sm = SourceMap::new();
        let file = sm.add_file("t.gw", src);
        let bytes = sm.get(file).unwrap().contents.as_bytes();
        let bump = Bump::new();
        let arena = FileArena::new(&bump, file);
        let (root, _diags) = parse(file, bytes, &arena);
        let block =
            find_comptime_block(root).expect("test source must contain a `comptime { ... }` block");
        let mut cx = EvalCx::with_budget(&sm, &NoBindings, budget);
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
    fn let_without_resolver_rejects() {
        // Under `NoBindings` the let-pattern lookup returns None, so
        // the evaluator can't store the value into a local slot.
        // Useful only as a contract check on the rejection path —
        // typeck supplies a real resolver and the end-to-end let
        // shape is covered by the phase2_comptime corpus.
        let err = eval_default("fn t() -> i32 { return comptime { let x = 1; x }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported (NoBindings can't resolve the let-pattern), got {err:?}",
        );
    }

    #[test]
    fn path_without_resolver_rejects() {
        // Same shape as above but exercises the eval_expr path-expr
        // arm directly: a path that didn't resolve to a local
        // produces a clear rejection rather than a wrong answer or
        // a panic.
        let err = eval_default("fn t() -> i32 { return comptime { x }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported (NoBindings can't resolve the path), got {err:?}",
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

    // CT.2e: short-circuit `&&` / `||` over bools.

    #[test]
    fn and_with_eager_path() {
        // LHS=true, doesn't short-circuit, so RHS evaluates and
        // determines the result.
        let b = assert_bool(
            eval_default("fn t() -> bool { return comptime { true && (3 > 2) }; }").unwrap(),
        );
        assert!(b);
    }

    #[test]
    fn and_short_circuits_on_false_lhs() {
        // LHS=false → short-circuit to false, RHS never runs.
        // The RHS is `1 / 0` which would raise
        // `EvalError::DivisionByZero` if evaluated; a passing test
        // proves the short-circuit fired.
        let b = assert_bool(
            eval_default("fn t() -> bool { return comptime { false && (1 / 0 == 0) }; }").unwrap(),
        );
        assert!(!b);
    }

    #[test]
    fn or_with_eager_path() {
        // LHS=false, doesn't short-circuit, so RHS evaluates.
        let b = assert_bool(
            eval_default("fn t() -> bool { return comptime { false || (3 > 2) }; }").unwrap(),
        );
        assert!(b);
    }

    #[test]
    fn or_short_circuits_on_true_lhs() {
        // LHS=true → short-circuit to true, RHS never runs. Same
        // `1 / 0` regression-net pattern as the `&&` case.
        let b = assert_bool(
            eval_default("fn t() -> bool { return comptime { true || (1 / 0 == 0) }; }").unwrap(),
        );
        assert!(b);
    }

    #[test]
    fn and_propagates_rhs_error_when_lhs_true() {
        // LHS=true means RHS evaluates and any error inside it
        // surfaces. Catches an asymmetric "always short-circuit"
        // miscompile that would mask the RHS's DivisionByZero.
        let err = eval_default("fn t() -> bool { return comptime { true && (1 / 0 == 0) }; }")
            .unwrap_err();
        assert!(
            matches!(err, EvalError::DivisionByZero(_)),
            "expected DivisionByZero (RHS must run when LHS is true), got {err:?}",
        );
    }

    #[test]
    fn or_propagates_rhs_error_when_lhs_false() {
        // Symmetric to the previous test for `||`. LHS=false →
        // RHS evaluates → DivisionByZero surfaces.
        let err = eval_default("fn t() -> bool { return comptime { false || (1 / 0 == 0) }; }")
            .unwrap_err();
        assert!(
            matches!(err, EvalError::DivisionByZero(_)),
            "expected DivisionByZero (RHS must run when LHS is false), got {err:?}",
        );
    }

    #[test]
    fn integer_lhs_in_logical_rejects() {
        // `1 && true` — integer LHS; expect_bool should reject.
        // Typeck would normally diagnose this before the
        // evaluator runs; the test exercises the evaluator's
        // contract independently.
        let err = eval_default("fn t() -> bool { return comptime { 1 && true }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported (non-bool LHS), got {err:?}",
        );
    }

    // CT.2d: `if`/`else` over CtValue::Bool.

    #[test]
    fn if_true_takes_then_arm() {
        let n = assert_int(
            eval_default("fn t() -> i32 { return comptime { (if true { 7 } else { 0 }) }; }")
                .unwrap(),
        );
        assert_eq!(n, 7);
    }

    #[test]
    fn if_false_takes_else_arm() {
        let n = assert_int(
            eval_default("fn t() -> i32 { return comptime { (if false { 7 } else { 99 }) }; }")
                .unwrap(),
        );
        assert_eq!(n, 99);
    }

    #[test]
    fn if_un_taken_arm_is_not_evaluated() {
        // The else arm contains `1 / 0`. Under proper branch-eval
        // discipline only the taken (then) arm runs and the
        // division by zero stays latent. If the evaluator
        // accidentally walked both arms it would raise
        // EvalError::DivisionByZero and this test would fail.
        let n = assert_int(
            eval_default("fn t() -> i32 { return comptime { (if true { 5 } else { 1 / 0 }) }; }")
                .unwrap(),
        );
        assert_eq!(n, 5);
    }

    #[test]
    fn if_un_taken_then_arm_is_not_evaluated() {
        // Mirror of the previous test, with the side-effecting
        // expression in the then arm. Catches an asymmetric
        // "only evaluate else arm" miscompile.
        let n = assert_int(
            eval_default("fn t() -> i32 { return comptime { (if false { 1 / 0 } else { 5 }) }; }")
                .unwrap(),
        );
        assert_eq!(n, 5);
    }

    #[test]
    fn if_else_if_chain_dispatches_correctly() {
        // First condition false, second true → take the middle
        // arm. Exercises the recursive `else_branch` returning
        // an `Expr::If` that re-enters eval_if through eval_expr.
        let n = assert_int(
            eval_default(
                "fn t() -> i32 { return comptime { (if false { 1 } else if true { 22 } else { 99 }) }; }",
            )
            .unwrap(),
        );
        assert_eq!(n, 22);
    }

    #[test]
    fn if_with_bool_result() {
        // Both arms produce CtValue::Bool. The condition is `1 < 2`
        // (true), then arm `true == true` (true).
        let b = assert_bool(
            eval_default(
                "fn t() -> bool { return comptime { (if 1 < 2 { true == true } else { false }) }; }",
            )
            .unwrap(),
        );
        assert!(b);
    }

    #[test]
    fn if_condition_must_be_bool() {
        // `if 1 { ... }` — integer condition; expect_bool should
        // reject. The corpus path goes through typeck which
        // rejects the integer condition with TYPE_MISMATCH; the
        // evaluator-level Unsupported is the defensive
        // last-resort message if a malformed program ever makes
        // it past typeck.
        let err = eval_default("fn t() -> i32 { return comptime { (if 1 { 7 } else { 0 }) }; }")
            .unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported (non-bool condition), got {err:?}",
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

    // ---- CT.3a: Float ----

    fn assert_float(v: CtValue) -> f64 {
        match v {
            CtValue::Float(f) => f,
            other => panic!("expected CtValue::Float, got {other:?}"),
        }
    }

    #[test]
    fn float_literal_parses() {
        let f = assert_float(
            eval_default("fn t() -> f64 { return comptime { 3.14 }; }").unwrap(),
        );
        assert_eq!(f, 3.14);
    }

    #[test]
    fn float_literal_with_underscores() {
        // Mirrors gw_mir's `raw.replace('_', "").parse::<f64>()`.
        let f = assert_float(
            eval_default("fn t() -> f64 { return comptime { 1_000.5 }; }").unwrap(),
        );
        assert_eq!(f, 1000.5);
    }

    #[test]
    fn float_negation() {
        let f = assert_float(
            eval_default("fn t() -> f64 { return comptime { -2.5 }; }").unwrap(),
        );
        assert_eq!(f, -2.5);
    }

    #[test]
    fn float_addition() {
        let f = assert_float(
            eval_default("fn t() -> f64 { return comptime { 1.5 + 2.25 }; }").unwrap(),
        );
        assert_eq!(f, 3.75);
    }

    #[test]
    fn float_subtraction() {
        let f = assert_float(
            eval_default("fn t() -> f64 { return comptime { 5.0 - 1.25 }; }").unwrap(),
        );
        assert_eq!(f, 3.75);
    }

    #[test]
    fn float_multiplication() {
        let f = assert_float(
            eval_default("fn t() -> f64 { return comptime { 1.5 * 4.0 }; }").unwrap(),
        );
        assert_eq!(f, 6.0);
    }

    #[test]
    fn float_division() {
        let f = assert_float(
            eval_default("fn t() -> f64 { return comptime { 7.5 / 2.5 }; }").unwrap(),
        );
        assert_eq!(f, 3.0);
    }

    #[test]
    fn float_modulo() {
        let f = assert_float(
            eval_default("fn t() -> f64 { return comptime { 5.5 % 2.0 }; }").unwrap(),
        );
        assert_eq!(f, 1.5);
    }

    #[test]
    fn float_division_by_zero_yields_infinity() {
        // IEEE-754: `1.0 / 0.0 = +∞`; no `DivisionByZero` error
        // (distinct from the integer path, which raises). This is the
        // canonical assertion of float division semantics.
        let f = assert_float(
            eval_default("fn t() -> f64 { return comptime { 1.0 / 0.0 }; }").unwrap(),
        );
        assert!(f.is_infinite() && f.is_sign_positive(), "expected +inf, got {f}");
    }

    #[test]
    fn float_zero_divided_by_zero_yields_nan() {
        // IEEE-754: `0.0 / 0.0 = NaN`. Used as the canonical NaN
        // source by the comptime corpus's NaN-ordering fixture.
        let f = assert_float(
            eval_default("fn t() -> f64 { return comptime { 0.0 / 0.0 }; }").unwrap(),
        );
        assert!(f.is_nan(), "expected NaN, got {f}");
    }

    #[test]
    fn float_precedence_threads_evaluator() {
        // Pratt precedence already pre-shapes the binary tree; the
        // evaluator just walks it. `2.0 + 3.0 * 4.0` → 14.0 confirms
        // the float path observes the same precedence as the int path.
        let f = assert_float(
            eval_default("fn t() -> f64 { return comptime { 2.0 + 3.0 * 4.0 }; }").unwrap(),
        );
        assert_eq!(f, 14.0);
    }

    #[test]
    fn float_lt_true() {
        let b = assert_bool(
            eval_default("fn t() -> bool { return comptime { 1.5 < 2.5 }; }").unwrap(),
        );
        assert!(b);
    }

    #[test]
    fn float_lt_false() {
        let b = assert_bool(
            eval_default("fn t() -> bool { return comptime { 2.5 < 1.5 }; }").unwrap(),
        );
        assert!(!b);
    }

    #[test]
    fn float_eq_true() {
        let b = assert_bool(
            eval_default("fn t() -> bool { return comptime { 1.25 == 1.25 }; }").unwrap(),
        );
        assert!(b);
    }

    #[test]
    fn float_nan_equality_is_false() {
        // IEEE-754: `NaN == NaN` is `false`. This is the most-cited
        // float foot-gun in real code; the comptime evaluator must
        // mirror runtime semantics here.
        let b = assert_bool(
            eval_default("fn t() -> bool { return comptime { (0.0 / 0.0) == (0.0 / 0.0) }; }")
                .unwrap(),
        );
        assert!(!b);
    }

    #[test]
    fn float_nan_ordering_is_false() {
        // IEEE-754: any ordering comparison involving NaN returns
        // `false`. The CT.2d-style `if` then takes the `else` arm.
        // This is the regression-proof that `<` on NaN doesn't
        // accidentally trip a Rust-side panic or wrong answer.
        let b = assert_bool(
            eval_default("fn t() -> bool { return comptime { (0.0 / 0.0) < 1.0 }; }").unwrap(),
        );
        assert!(!b);
    }

    #[test]
    fn mixed_int_float_arithmetic_rejects() {
        // Runtime requires an explicit `as f64` cast for the int side;
        // the comptime evaluator mirrors that rule rather than
        // inventing an int-to-float coercion.
        let err = eval_default("fn t() -> f64 { return comptime { 1 + 1.0 }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported (mixed int+float), got {err:?}",
        );
    }

    #[test]
    fn mixed_int_float_equality_rejects() {
        let err =
            eval_default("fn t() -> bool { return comptime { 1 == 1.0 }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported (mixed int==float), got {err:?}",
        );
    }

    #[test]
    fn float_arith_with_bool_rejects() {
        // Bool can't participate in float arithmetic; this checks the
        // tuple-dispatch's catch-all rejection path rather than the
        // (now-removed) expect_int helper.
        let err =
            eval_default("fn t() -> f64 { return comptime { 1.0 + true }; }").unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported (float+bool), got {err:?}",
        );
    }

    #[test]
    fn parse_float_literal_handles_underscores() {
        assert_eq!(parse_float_literal("3.14"), Some(3.14));
        assert_eq!(parse_float_literal("1_000.5"), Some(1000.5));
    }

    #[test]
    fn parse_float_literal_rejects_garbage() {
        assert_eq!(parse_float_literal("not-a-float"), None);
    }

    // ---- CT.3b: String literals ----

    fn assert_str(v: CtValue) -> Vec<u8> {
        match v {
            CtValue::Str(b) => b,
            other => panic!("expected CtValue::Str, got {other:?}"),
        }
    }

    #[test]
    fn string_literal_decodes() {
        let bytes = assert_str(
            eval_default("fn t() -> []u8 { return comptime { \"hello\" }; }").unwrap(),
        );
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn string_literal_empty() {
        let bytes = assert_str(
            eval_default("fn t() -> []u8 { return comptime { \"\" }; }").unwrap(),
        );
        assert_eq!(bytes, b"");
    }

    #[test]
    fn string_literal_with_newline_escape() {
        // Pins the lockstep with `gw_mir::decode_string_literal` —
        // `\n` decodes to byte 0x0A, not the two characters `\` `n`.
        let bytes = assert_str(
            eval_default("fn t() -> []u8 { return comptime { \"hi\\n\" }; }").unwrap(),
        );
        assert_eq!(bytes, b"hi\n");
    }

    #[test]
    fn string_literal_with_tab_and_backslash_escapes() {
        // Exercises three of the seven supported escapes in one
        // payload — covers the canonical lockstep set without
        // duplicating per-escape boilerplate.
        let bytes = assert_str(
            eval_default(
                "fn t() -> []u8 { return comptime { \"a\\tb\\\\c\\\"d\" }; }",
            )
            .unwrap(),
        );
        assert_eq!(bytes, b"a\tb\\c\"d");
    }

    #[test]
    fn string_literal_unknown_escape_passes_through() {
        // Mirrors `gw_mir::decode_string_literal`'s "unknown escapes
        // pass through literally" rule. `\q` is not in the supported
        // set, so the decoder emits backslash + 'q'.
        let bytes = assert_str(
            eval_default("fn t() -> []u8 { return comptime { \"\\q\" }; }").unwrap(),
        );
        assert_eq!(bytes, b"\\q");
    }

    #[test]
    fn arithmetic_on_string_rejects() {
        // No comptime operations on strings yet — concat / `==` /
        // `.len` ride a future sub-bundle. This pins the tuple-
        // dispatch's catch-all rejection so future scope expansions
        // don't silently start accepting an undefined shape.
        let err = eval_default(
            "fn t() -> []u8 { return comptime { \"a\" + \"b\" }; }",
        )
        .unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { .. }),
            "expected Unsupported (string + string), got {err:?}",
        );
    }

    #[test]
    fn decode_string_literal_matches_runtime() {
        // The canonical lockstep assertion: every byte the comptime
        // evaluator decodes must match what `gw_mir`'s runtime
        // decoder produces for the same token text. If this test
        // ever fails, either both decoders need updating in
        // lockstep, or `gw_comptime` and `gw_mir` have drifted.
        assert_eq!(decode_string_literal("\"hello\""), b"hello");
        assert_eq!(decode_string_literal("\"hi\\n\""), b"hi\n");
        assert_eq!(decode_string_literal("\"\\t\\r\\0\""), b"\t\r\0");
        assert_eq!(decode_string_literal("\"\\\\\""), b"\\");
        assert_eq!(decode_string_literal("\"\""), b"");
    }
}
