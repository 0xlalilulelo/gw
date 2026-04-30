//! GW Mid-level IR — Phase 1 minimum.
//!
//! See `docs/architecture.md` Part B.7 (MIR builder) and Part C.3
//! (MIR shape). The Phase-1 slice models functions as control-flow
//! graphs of basic blocks with three-address statements; full SSA
//! construction with dominance frontiers (Cytron et al.) is **not**
//! performed here — the Cranelift backend imposes its own SSA
//! discipline via `Variable`, so the front end's MIR can stay simpler.
//! When a future LLVM backend or borrow checker needs proper SSA, that
//! conversion lands in this crate without changing consumers.
//!
//! Increment 1 of Phase 1 only exercises `Return(Const)`; the rest of
//! the IR is filled in to support increments 2–6 without churn.

use arsenal_ast::{
    AstNode, BinaryExpr, Block, CallExpr, Expr, ExprStmt, FnDecl, IfExpr, LetStmt, LiteralExpr,
    ParenExpr, PathExpr, Pattern, ReturnExpr, Stmt, SyntaxKind, UnaryExpr, WhileExpr,
};
use arsenal_lex::{SourceMap, Span};
use arsenal_resolve::{DefId, ResolvedModule};
use arsenal_typeck::{BindingId, FloatTy, FnSig, IntTy, NodePtr, Ty, TypedModule};
use rustc_hash::FxHashMap;

/// The whole Phase-1 program: a flat list of functions.
#[derive(Debug)]
pub struct MirProgram {
    /// Functions in the order their [`DefId`]s were assigned.
    pub functions: Vec<MirFn>,
}

/// One lowered function.
#[derive(Debug)]
pub struct MirFn {
    /// Source-level identifier (used as the symbol name by codegen).
    pub name: String,
    /// Parameter types and their [`Local`] indices (always 0..N).
    pub params: Vec<Local>,
    /// Return type. `Ty::U0` means "no return value".
    pub return_ty: Ty,
    /// All local slots, in declaration order. Indices 0..params.len()
    /// are parameters; the rest are `let`-bindings or compiler-
    /// synthesised temporaries.
    pub locals: Vec<LocalDecl>,
    /// Basic blocks; `blocks[0]` is the entry block.
    pub blocks: Vec<MirBlock>,
}

/// A local slot.
#[derive(Copy, Clone, Debug)]
pub struct LocalDecl {
    /// Type of the local.
    pub ty: Ty,
    /// Source span of the declaration (for diagnostics; may be
    /// synthetic for temporaries).
    pub span: Span,
}

/// Index into [`MirFn::locals`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Local(pub u32);

/// Index into [`MirFn::blocks`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BlockId(pub u32);

/// One basic block.
#[derive(Debug)]
pub struct MirBlock {
    /// Sequence of statements executed in order.
    pub statements: Vec<MirStmt>,
    /// How control leaves this block.
    pub terminator: Terminator,
}

/// A within-block statement.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub enum MirStmt {
    /// `dst = rvalue`.
    Assign { dst: Local, value: Rvalue },
}

/// Right-hand side of a [`MirStmt::Assign`].
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub enum Rvalue {
    /// Direct read of an operand.
    Use(Operand),
    /// Binary operation `lhs op rhs`. `ty` is the operand type used for
    /// codegen (which equals the result type for arithmetic, and the
    /// operand type for comparisons that always yield `bool`).
    BinOp {
        op: BinOp,
        lhs: Operand,
        rhs: Operand,
        ty: Ty,
    },
    /// Unary operation `op operand`. `ty` is the operand type.
    UnOp { op: UnOp, operand: Operand, ty: Ty },
}

/// Reference to a value: either a constant or a local.
#[derive(Debug, Clone)]
pub enum Operand {
    /// A literal constant.
    Const(Const),
    /// A local slot (parameter, let-binding, or temporary).
    Local(Local),
}

/// Concrete constant.
#[derive(Debug, Clone, Copy)]
#[allow(missing_docs)]
pub enum Const {
    /// Integer constant — value held at maximum precision and narrowed
    /// to `ty` at codegen time.
    Int {
        value: i128,
        ty: IntTy,
    },
    /// Floating-point constant — `bits` is the IEEE-754 bit pattern in
    /// `ty`'s representation (for `f32`, the low 32 bits are valid).
    Float {
        bits: u64,
        ty: FloatTy,
    },
    Bool(bool),
    /// `u0` unit value.
    Unit,
    /// Placeholder when lowering encountered an error.
    Error,
}

/// Block terminator.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub enum Terminator {
    /// Unconditional branch.
    Goto(BlockId),
    /// Boolean branch.
    Branch {
        /// Condition; must have type `bool`.
        cond: Operand,
        /// Branch taken when `cond` is `true`.
        then_bb: BlockId,
        /// Branch taken when `cond` is `false`.
        else_bb: BlockId,
    },
    /// Return from the function.
    Return(Operand),
    /// Function call; on return, control passes to `target_bb` and the
    /// result lands in `dst` (or `dst.ty == Ty::U0` if the callee
    /// returns nothing).
    Call {
        /// Callee. Phase 1 only models calls to top-level fns by index
        /// into [`MirProgram::functions`].
        callee: FnIdx,
        /// Argument operands, in callee parameter order.
        args: Vec<Operand>,
        /// Where the result lands.
        dst: Local,
        /// Continuation block.
        target_bb: BlockId,
    },
    /// Unreachable code (e.g. tail of a block whose last stmt was a
    /// `return`).
    Unreachable,
}

/// Index into [`MirProgram::functions`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct FnIdx(pub u32);

/// Binary operator kinds.
#[allow(missing_docs)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    LogAnd,
    LogOr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Unary operator kinds.
#[allow(missing_docs)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

// ─── lowering ─────────────────────────────────────────────────────────

/// Lower a [`TypedModule`] to a [`MirProgram`].
///
/// Lowering is best-effort: when the type checker recorded an error
/// type for an expression, the lowered MIR uses [`Const::Error`] as a
/// placeholder so codegen can still emit something.
pub fn lower<'a>(
    typed: &TypedModule<'a>,
    resolved: &ResolvedModule<'a>,
    sm: &SourceMap,
) -> MirProgram {
    let mut def_to_fn: FxHashMap<DefId, FnIdx> = FxHashMap::default();
    for (i, def) in resolved.defs.iter().enumerate() {
        def_to_fn.insert(def.id, FnIdx(i as u32));
    }

    let mut functions = Vec::with_capacity(resolved.defs.len());
    for def in &resolved.defs {
        let fn_decl = FnDecl::cast(def.syntax).expect("resolver only registers FnDecl");
        let sig = typed.sigs.get(&def.id).cloned().unwrap_or(FnSig {
            params: Vec::new(),
            ret: Ty::Error,
        });
        let mir_fn = lower_fn(def.name.clone(), fn_decl, &sig, typed, sm, &def_to_fn);
        functions.push(mir_fn);
    }
    MirProgram { functions }
}

fn lower_fn<'a>(
    name: String,
    fn_decl: FnDecl<'a>,
    sig: &FnSig,
    typed: &TypedModule<'a>,
    sm: &SourceMap,
    def_to_fn: &FxHashMap<DefId, FnIdx>,
) -> MirFn {
    let mut b = Builder::new();

    // Allocate parameter locals first so their indices are 0..params.
    let mut binding_to_local: FxHashMap<BindingId, Local> = FxHashMap::default();
    for p in &sig.params {
        let local = b.alloc_local(LocalDecl {
            ty: p.ty,
            span: p.name_span,
        });
        b.params.push(local);
        // Resolver/typeck assign BindingIds in source order, matching
        // our Local allocation order, but we still record explicitly so
        // intra-body PathExpr lookups work.
        binding_to_local.insert(BindingId(local.0), local);
    }

    // Open the entry block.
    let entry = b.alloc_block();
    b.cur = entry;

    if let Some(body) = fn_decl.body() {
        let mut lcx = LowerCx {
            typed,
            sm,
            def_to_fn,
            binding_to_local,
        };
        let _ = lower_block(&mut b, body, &mut lcx);
    }

    // If the entry block (or wherever we ended up) has no terminator
    // yet, fabricate one. This happens when the function body falls
    // through without an explicit `return`. For a function returning
    // `u0` this is fine; for any other return type it would have been
    // a type-checker error already, and the unreachable terminator is
    // a safety net.
    if b.blocks[b.cur.0 as usize].terminator_set {
        // already set
    } else if sig.ret == Ty::U0 || sig.ret == Ty::Error {
        b.set_terminator(b.cur, Terminator::Return(Operand::Const(Const::Unit)));
    } else {
        b.set_terminator(b.cur, Terminator::Unreachable);
    }

    MirFn {
        name,
        params: b.params,
        return_ty: sig.ret,
        locals: b.locals,
        blocks: b.blocks.into_iter().map(|x| x.into_block()).collect(),
    }
}

// ─── builder ──────────────────────────────────────────────────────────

struct Builder {
    params: Vec<Local>,
    locals: Vec<LocalDecl>,
    blocks: Vec<DraftBlock>,
    /// Currently-open block. New statements go into this block.
    cur: BlockId,
}

struct DraftBlock {
    statements: Vec<MirStmt>,
    terminator: Terminator,
    terminator_set: bool,
}

impl DraftBlock {
    fn new() -> Self {
        Self {
            statements: Vec::new(),
            terminator: Terminator::Unreachable,
            terminator_set: false,
        }
    }
    fn into_block(self) -> MirBlock {
        MirBlock {
            statements: self.statements,
            terminator: self.terminator,
        }
    }
}

impl Builder {
    fn new() -> Self {
        Self {
            params: Vec::new(),
            locals: Vec::new(),
            blocks: Vec::new(),
            cur: BlockId(0),
        }
    }

    fn alloc_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(DraftBlock::new());
        id
    }

    fn alloc_local(&mut self, decl: LocalDecl) -> Local {
        let id = Local(self.locals.len() as u32);
        self.locals.push(decl);
        id
    }

    fn push_stmt(&mut self, stmt: MirStmt) {
        let bb = &mut self.blocks[self.cur.0 as usize];
        if bb.terminator_set {
            // Statements after a terminator are unreachable; drop them
            // silently. Type checking should have caught the dead-code
            // case, but the IR stays sound regardless.
            return;
        }
        bb.statements.push(stmt);
    }

    fn set_terminator(&mut self, bb: BlockId, term: Terminator) {
        let blk = &mut self.blocks[bb.0 as usize];
        if !blk.terminator_set {
            blk.terminator = term;
            blk.terminator_set = true;
        }
    }
}

struct LowerCx<'a, 'tm, 'sm, 'm> {
    typed: &'tm TypedModule<'a>,
    sm: &'sm SourceMap,
    def_to_fn: &'m FxHashMap<DefId, FnIdx>,
    binding_to_local: FxHashMap<BindingId, Local>,
}

fn lower_block<'a>(
    b: &mut Builder,
    block: Block<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_>,
) -> Operand {
    for stmt in block.stmts() {
        lower_stmt(b, stmt, lcx);
    }
    if let Some(tail) = block.tail_expr() {
        lower_expr(b, tail, lcx)
    } else {
        Operand::Const(Const::Unit)
    }
}

fn lower_stmt<'a>(b: &mut Builder, stmt: Stmt<'a>, lcx: &mut LowerCx<'a, '_, '_, '_>) {
    match stmt {
        Stmt::Let(l) => lower_let(b, l, lcx),
        Stmt::Expr(es) => lower_expr_stmt(b, es, lcx),
        Stmt::Stub(_) | Stmt::Error(_) => {}
    }
}

fn lower_let<'a>(b: &mut Builder, let_stmt: LetStmt<'a>, lcx: &mut LowerCx<'a, '_, '_, '_>) {
    // Determine the local's type from typeck. We re-resolve via the
    // pattern's binding id which the typeck context allocated in source
    // order — i.e. it equals the Local index we'll allocate now.
    let init_ty = let_stmt
        .init()
        .and_then(|e| lcx.typed.expr_types.get(&NodePtr(e.syntax())).copied())
        .unwrap_or(Ty::Error);
    let ty = let_stmt
        .ty()
        .and_then(|t| primitive_from_ast(t, lcx.sm))
        .unwrap_or(init_ty);

    match let_stmt.pattern() {
        Some(Pattern::Ident(p)) => {
            let span = p.name().unwrap_or_else(Span::synthetic);
            let local = b.alloc_local(LocalDecl { ty, span });
            // BindingId mirrors the typeck allocator: param count + #lets
            // so far. We use the local's own index as the BindingId by
            // construction (typeck assigns them in lock-step source
            // order, matching our allocation order).
            lcx.binding_to_local.insert(BindingId(local.0), local);
            if let Some(init) = let_stmt.init() {
                let val = lower_expr(b, init, lcx);
                b.push_stmt(MirStmt::Assign {
                    dst: local,
                    value: Rvalue::Use(val),
                });
            }
        }
        Some(Pattern::Wildcard(_)) => {
            // Allocate a binding-id slot but no local; if there's an
            // init, lower it for side effects and discard.
            if let Some(init) = let_stmt.init() {
                let _ = lower_expr(b, init, lcx);
            }
        }
        _ => {}
    }
}

fn lower_expr_stmt<'a>(b: &mut Builder, es: ExprStmt<'a>, lcx: &mut LowerCx<'a, '_, '_, '_>) {
    if let Some(e) = es.expr() {
        let _ = lower_expr(b, e, lcx);
    }
}

fn lower_expr<'a>(b: &mut Builder, expr: Expr<'a>, lcx: &mut LowerCx<'a, '_, '_, '_>) -> Operand {
    match expr {
        Expr::Literal(l) => lower_literal(l, lcx),
        Expr::Path(p) => lower_path(p, lcx),
        Expr::Paren(p) => lower_paren(b, p, lcx),
        Expr::Binary(bin) => lower_binary(b, bin, lcx),
        Expr::Unary(u) => lower_unary(b, u, lcx),
        Expr::Block(blk) => lower_block(b, blk, lcx),
        Expr::If(i) => lower_if(b, i, lcx),
        Expr::While(w) => lower_while(b, w, lcx),
        Expr::Return(r) => lower_return(b, r, lcx),
        Expr::Call(c) => lower_call(b, c, lcx),
        Expr::Stub(_) | Expr::Error(_) => Operand::Const(Const::Error),
    }
}

fn lower_literal<'a>(l: LiteralExpr<'a>, lcx: &mut LowerCx<'a, '_, '_, '_>) -> Operand {
    let ty = lcx
        .typed
        .expr_types
        .get(&NodePtr(l.syntax()))
        .copied()
        .unwrap_or(Ty::Error);
    let raw = lcx.sm.slice(l.syntax().span).unwrap_or("");
    match l.token_kind() {
        Some(SyntaxKind::IntLit) => {
            let int_ty = match ty {
                Ty::Int(i) => i,
                _ => IntTy::I32,
            };
            let value = parse_int_literal(raw).unwrap_or(0);
            Operand::Const(Const::Int { value, ty: int_ty })
        }
        Some(SyntaxKind::KwTrue) => Operand::Const(Const::Bool(true)),
        Some(SyntaxKind::KwFalse) => Operand::Const(Const::Bool(false)),
        Some(SyntaxKind::FloatLit) => {
            let float_ty = match ty {
                Ty::Float(f) => f,
                _ => FloatTy::F64,
            };
            let parsed: f64 = raw.replace('_', "").parse().unwrap_or(0.0);
            let bits = match float_ty {
                FloatTy::F64 => parsed.to_bits(),
                FloatTy::F32 => (parsed as f32).to_bits() as u64,
            };
            Operand::Const(Const::Float { bits, ty: float_ty })
        }
        // Strings, runes, byte chars — Phase 1 doesn't yet handle.
        _ => Operand::Const(Const::Error),
    }
}

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

fn lower_path<'a>(p: PathExpr<'a>, lcx: &mut LowerCx<'a, '_, '_, '_>) -> Operand {
    if let Some(binding_id) = lcx.typed.path_bindings.get(&NodePtr(p.syntax())) {
        if let Some(local) = lcx.binding_to_local.get(binding_id) {
            return Operand::Local(*local);
        }
    }
    Operand::Const(Const::Error)
}

fn lower_paren<'a>(
    b: &mut Builder,
    p: ParenExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_>,
) -> Operand {
    p.inner()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Error))
}

fn lower_binary<'a>(
    b: &mut Builder,
    bin: BinaryExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_>,
) -> Operand {
    let lhs = bin
        .lhs()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Error));
    let rhs = bin
        .rhs()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Error));
    let result_ty = lcx
        .typed
        .expr_types
        .get(&NodePtr(bin.syntax()))
        .copied()
        .unwrap_or(Ty::Error);
    let Some(op) = bin.op_kind().and_then(syntax_kind_to_binop) else {
        return Operand::Const(Const::Error);
    };
    // Operand type is the comparison's input type, which equals the
    // result type for arithmetic and equals lhs's type otherwise.
    let value = Rvalue::BinOp {
        op,
        lhs,
        rhs,
        ty: result_ty,
    };
    let local = b.alloc_local(LocalDecl {
        ty: result_ty,
        span: bin.syntax().span,
    });
    b.push_stmt(MirStmt::Assign { dst: local, value });
    Operand::Local(local)
}

fn lower_unary<'a>(
    b: &mut Builder,
    u: UnaryExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_>,
) -> Operand {
    let operand = u
        .operand()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Error));
    let result_ty = lcx
        .typed
        .expr_types
        .get(&NodePtr(u.syntax()))
        .copied()
        .unwrap_or(Ty::Error);
    let op = match u.op_kind() {
        Some(SyntaxKind::Minus) => UnOp::Neg,
        Some(SyntaxKind::Bang) => UnOp::Not,
        Some(SyntaxKind::Tilde) => UnOp::BitNot,
        _ => return Operand::Const(Const::Error),
    };
    let value = Rvalue::UnOp {
        op,
        operand,
        ty: result_ty,
    };
    let local = b.alloc_local(LocalDecl {
        ty: result_ty,
        span: u.syntax().span,
    });
    b.push_stmt(MirStmt::Assign { dst: local, value });
    Operand::Local(local)
}

fn lower_if<'a>(b: &mut Builder, i: IfExpr<'a>, lcx: &mut LowerCx<'a, '_, '_, '_>) -> Operand {
    // cond
    let cond = i
        .cond()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Bool(false)));
    let result_ty = lcx
        .typed
        .expr_types
        .get(&NodePtr(i.syntax()))
        .copied()
        .unwrap_or(Ty::U0);

    let then_bb = b.alloc_block();
    let else_bb = b.alloc_block();
    let join_bb = b.alloc_block();

    // Emit the branch from the current block.
    b.set_terminator(
        b.cur,
        Terminator::Branch {
            cond,
            then_bb,
            else_bb,
        },
    );

    // The result of the if-expression lives in a fresh local that both
    // arms write to before jumping to the join. For Ty::U0 we still
    // allocate a Unit slot for uniformity.
    let result_local = b.alloc_local(LocalDecl {
        ty: result_ty,
        span: i.syntax().span,
    });

    // Then arm.
    b.cur = then_bb;
    let then_val = i
        .then_block()
        .map(|blk| lower_block(b, blk, lcx))
        .unwrap_or(Operand::Const(Const::Unit));
    b.push_stmt(MirStmt::Assign {
        dst: result_local,
        value: Rvalue::Use(then_val),
    });
    b.set_terminator(b.cur, Terminator::Goto(join_bb));

    // Else arm.
    b.cur = else_bb;
    let else_val = match i.else_branch() {
        Some(branch) => lower_expr(b, branch, lcx),
        None => Operand::Const(Const::Unit),
    };
    b.push_stmt(MirStmt::Assign {
        dst: result_local,
        value: Rvalue::Use(else_val),
    });
    b.set_terminator(b.cur, Terminator::Goto(join_bb));

    // Continue lowering at the join block.
    b.cur = join_bb;
    Operand::Local(result_local)
}

fn lower_while<'a>(
    b: &mut Builder,
    w: WhileExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_>,
) -> Operand {
    let header_bb = b.alloc_block();
    let body_bb = b.alloc_block();
    let exit_bb = b.alloc_block();

    b.set_terminator(b.cur, Terminator::Goto(header_bb));

    // Condition evaluation lives in the header block.
    b.cur = header_bb;
    let cond = w
        .cond()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Bool(false)));
    b.set_terminator(
        b.cur,
        Terminator::Branch {
            cond,
            then_bb: body_bb,
            else_bb: exit_bb,
        },
    );

    // Body.
    b.cur = body_bb;
    if let Some(body) = w.body() {
        let _ = lower_block(b, body, lcx);
    }
    b.set_terminator(b.cur, Terminator::Goto(header_bb));

    // Continue at exit.
    b.cur = exit_bb;
    Operand::Const(Const::Unit)
}

fn lower_return<'a>(
    b: &mut Builder,
    r: ReturnExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_>,
) -> Operand {
    let val = match r.value() {
        Some(e) => lower_expr(b, e, lcx),
        None => Operand::Const(Const::Unit),
    };
    b.set_terminator(b.cur, Terminator::Return(val));
    // Subsequent code in this block is unreachable; allocate a fresh
    // block for it so any further appends don't clobber the terminator.
    let dead = b.alloc_block();
    b.cur = dead;
    Operand::Const(Const::Error)
}

fn lower_call<'a>(b: &mut Builder, c: CallExpr<'a>, lcx: &mut LowerCx<'a, '_, '_, '_>) -> Operand {
    let Some(target) = lcx.typed.call_targets.get(&NodePtr(c.syntax())).copied() else {
        return Operand::Const(Const::Error);
    };
    let Some(fn_idx) = lcx.def_to_fn.get(&target).copied() else {
        return Operand::Const(Const::Error);
    };
    let mut args = Vec::new();
    if let Some(arg_list) = c.args() {
        for a in arg_list.args() {
            args.push(lower_expr(b, a, lcx));
        }
    }
    let result_ty = lcx
        .typed
        .expr_types
        .get(&NodePtr(c.syntax()))
        .copied()
        .unwrap_or(Ty::Error);
    let dst = b.alloc_local(LocalDecl {
        ty: result_ty,
        span: c.syntax().span,
    });
    let cont = b.alloc_block();
    b.set_terminator(
        b.cur,
        Terminator::Call {
            callee: fn_idx,
            args,
            dst,
            target_bb: cont,
        },
    );
    b.cur = cont;
    Operand::Local(dst)
}

// ─── helpers ──────────────────────────────────────────────────────────

fn syntax_kind_to_binop(k: SyntaxKind) -> Option<BinOp> {
    use SyntaxKind::*;
    Some(match k {
        Plus => BinOp::Add,
        Minus => BinOp::Sub,
        Star => BinOp::Mul,
        Slash => BinOp::Div,
        Percent => BinOp::Mod,
        StarStar => BinOp::Pow,
        Amp => BinOp::BitAnd,
        Pipe => BinOp::BitOr,
        Caret => BinOp::BitXor,
        LtLt => BinOp::Shl,
        GtGt => BinOp::Shr,
        AmpAmp => BinOp::LogAnd,
        PipePipe => BinOp::LogOr,
        EqEq => BinOp::Eq,
        BangEq => BinOp::Ne,
        Lt => BinOp::Lt,
        LtEq => BinOp::Le,
        Gt => BinOp::Gt,
        GtEq => BinOp::Ge,
        _ => return None,
    })
}

fn primitive_from_ast(ty: arsenal_ast::Type<'_>, sm: &SourceMap) -> Option<Ty> {
    let path = match ty {
        arsenal_ast::Type::Path(p) => p,
        _ => return None,
    };
    let mut segs = path.segments();
    let first = segs.next()?;
    if segs.next().is_some() {
        return None;
    }
    let name = sm.slice(first)?;
    arsenal_resolve::primitive_type_name(name).map(|p| match p {
        arsenal_resolve::PrimitiveTy::U0 => Ty::U0,
        arsenal_resolve::PrimitiveTy::Bool => Ty::Bool,
        arsenal_resolve::PrimitiveTy::I8 => Ty::Int(IntTy::I8),
        arsenal_resolve::PrimitiveTy::I16 => Ty::Int(IntTy::I16),
        arsenal_resolve::PrimitiveTy::I32 => Ty::Int(IntTy::I32),
        arsenal_resolve::PrimitiveTy::I64 => Ty::Int(IntTy::I64),
        arsenal_resolve::PrimitiveTy::U8 => Ty::Int(IntTy::U8),
        arsenal_resolve::PrimitiveTy::U16 => Ty::Int(IntTy::U16),
        arsenal_resolve::PrimitiveTy::U32 => Ty::Int(IntTy::U32),
        arsenal_resolve::PrimitiveTy::U64 => Ty::Int(IntTy::U64),
        arsenal_resolve::PrimitiveTy::ISize => Ty::Int(IntTy::ISize),
        arsenal_resolve::PrimitiveTy::USize => Ty::Int(IntTy::USize),
        arsenal_resolve::PrimitiveTy::F32 => Ty::Float(FloatTy::F32),
        arsenal_resolve::PrimitiveTy::F64 => Ty::Float(FloatTy::F64),
        arsenal_resolve::PrimitiveTy::Rune => Ty::Rune,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arsenal_ast::FileArena;
    use arsenal_parse::parse;
    use arsenal_resolve::resolve_module;
    use arsenal_typeck::type_check;
    use bumpalo::Bump;

    fn lower_src(src: &str) -> MirProgram {
        let mut sm = SourceMap::new();
        let file = sm.add_file("t.gw", src);
        let bytes = sm.get(file).unwrap().contents.as_bytes();
        let bump = Bump::new();
        let arena = FileArena::new(&bump, file);
        let (root, mut diags) = parse(file, bytes, &arena);
        let resolved = resolve_module(root, &sm, &mut diags);
        let typed = type_check(&resolved, &sm, &mut diags);
        assert!(!diags.has_errors(), "fixture should typecheck cleanly");
        lower(&typed, &resolved, &sm)
    }

    #[test]
    fn return_zero_one_function_one_block() {
        let prog = lower_src("fn main() -> i32 { return 0; }");
        assert_eq!(prog.functions.len(), 1);
        let f = &prog.functions[0];
        assert_eq!(f.name, "main");
        assert_eq!(f.return_ty, Ty::Int(IntTy::I32));
        // Entry block ends with Return(0).
        let entry = &f.blocks[0];
        match &entry.terminator {
            Terminator::Return(Operand::Const(Const::Int { value, ty })) => {
                assert_eq!(*value, 0);
                assert_eq!(*ty, IntTy::I32);
            }
            other => panic!("expected Return(Const::Int 0), got {other:?}"),
        }
    }

    #[test]
    fn return_one_plus_two_emits_binop() {
        let prog = lower_src("fn main() -> i32 { return 1 + 2; }");
        let f = &prog.functions[0];
        let entry = &f.blocks[0];
        // Should contain at least one Assign with BinOp Add.
        let has_add = entry.statements.iter().any(|s| {
            matches!(
                s,
                MirStmt::Assign {
                    value: Rvalue::BinOp { op: BinOp::Add, .. },
                    ..
                }
            )
        });
        assert!(has_add, "expected BinOp::Add in entry block");
    }
}
