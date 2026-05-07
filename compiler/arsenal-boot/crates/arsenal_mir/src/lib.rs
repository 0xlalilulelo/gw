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
    AstNode, BinaryExpr, Block, CallExpr, CastExpr, Expr, ExprStmt, FieldExpr, FnDecl, ForExpr,
    IfExpr, LetStmt, LiteralExpr, Module, ParenExpr, PathExpr, Pattern, ReturnExpr, Stmt,
    StructLitExpr, SyntaxKind, UnaryExpr, WhileExpr,
};
use arsenal_lex::{SourceMap, Span};
use arsenal_resolve::{DefId, DefKind, ResolvedModule};
use arsenal_typeck::{BindingId, ClassLayout, FloatTy, FnSig, IntTy, NodePtr, Ty, TypedModule};
use rustc_hash::FxHashMap;

/// The whole Phase-1 program: a flat list of functions, the class
/// layout table copied from typeck, and any string-literal payloads
/// that need to land in `.rodata` (Phase 1 increment 11b).
#[derive(Debug)]
pub struct MirProgram {
    /// Functions in the order their [`DefId`]s were assigned.
    pub functions: Vec<MirFn>,
    /// Class layouts indexed by [`DefId`]. Used by codegen to compute
    /// stack-slot sizes and field byte offsets.
    pub class_layouts: FxHashMap<DefId, ClassLayout>,
    /// Bytes of each string literal referenced by [`Const::DataAddr`].
    /// Indices are [`StringLitId`]s. Codegen materialises one Cranelift
    /// data object per entry.
    pub string_literals: Vec<Vec<u8>>,
}

/// Index into [`MirProgram::string_literals`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct StringLitId(pub u32);

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
    /// Basic blocks; `blocks[0]` is the entry block. **Empty for
    /// extern declarations** — the body lives in another translation
    /// unit (typically libc); codegen declares such functions with
    /// import-style linkage and skips body emission.
    pub blocks: Vec<MirBlock>,
    /// Whether this function has a body in this translation unit.
    /// `false` for `extern fn name(...) -> T;` declarations.
    pub is_extern: bool,
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
    /// `local.field = rvalue` — store into a field of a class-typed
    /// local. Distinct from `Assign` because the dst is a *projection*
    /// rather than the full local.
    AssignField {
        dst: Local,
        field_idx: u32,
        value: Rvalue,
    },
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
    /// Read of a field from a class-typed local: `local.field`.
    /// `field_ty` is the field's type (used by codegen for load width).
    Field {
        base: Local,
        field_idx: u32,
        field_ty: Ty,
    },
    /// `expr as Type` value cast. `kind` selects the codegen op;
    /// `src_ty` is the operand type (so codegen can read at the
    /// correct Cranelift width) and `dst_ty` is the result type.
    Cast {
        kind: CastKind,
        operand: Operand,
        src_ty: Ty,
        dst_ty: Ty,
    },
}

/// Kind of a [`Rvalue::Cast`].
///
/// Each variant maps to exactly one Cranelift op (or no op for the
/// `*Bitcast` cases), so codegen is a flat match. The dispatching
/// information — operand signedness, source-vs-destination widths —
/// lives here; codegen does not re-read [`Ty`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum CastKind {
    /// Source int narrower than destination int. `signed` tracks the
    /// **operand**'s signedness, which determines the correct
    /// extension: `true` → `sextend`, `false` → `uextend`.
    IntWiden { signed: bool },
    /// Source int wider than destination int — codegen lowers to
    /// `ireduce`, which keeps the low bits and discards the high
    /// ones. Lossy by construction.
    IntTrunc,
    /// Same bit width, signedness reinterpretation only (`i32 as u32`,
    /// `u8 as i8`). No codegen op needed — Cranelift integer types
    /// don't carry signedness, so the operand value is reused as-is.
    IntBitcast,
    /// Int → float. `signed` tracks the **operand**'s signedness:
    /// `true` → `fcvt_from_sint`, `false` → `fcvt_from_uint`.
    IntToFloat { signed: bool },
    /// Float → int with saturation + NaN-to-zero (matches Rust ≥ 1.45
    /// `as`). `signed` tracks the **destination**'s signedness:
    /// `true` → `fcvt_to_sint_sat`, `false` → `fcvt_to_uint_sat`.
    FloatToInt { signed: bool },
    /// f32 → f64 via `fpromote`.
    FloatExt,
    /// f64 → f32 via `fdemote`.
    FloatTrunc,
    /// Same float width on both sides (`f32 as f32`, `f64 as f64`).
    /// No codegen op needed; included for symmetry with `IntBitcast`.
    FloatBitcast,
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
    /// Pointer-sized address of a `.rodata` payload. The id is an
    /// index into [`MirProgram::string_literals`]. Codegen lowers via
    /// `module.declare_data_in_func` + `ins.global_value`.
    DataAddr(StringLitId),
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

    // Phase 1 increment 11c: a bare string literal at statement
    // position desugars to a `write(1, str.data, str.len)` call. We
    // either reuse the user's `extern fn write` declaration if they
    // wrote one (assuming the conventional libc signature) or inject a
    // synthetic extern at the end of `functions`.
    let needs_print = module_needs_print_desugar(resolved);
    let user_write_fnidx = find_user_write_extern(resolved, &def_to_fn);
    let real_fn_count = resolved
        .defs
        .iter()
        .filter(|d| matches!(d.kind, DefKind::Fn | DefKind::SyntheticMain))
        .count();
    let print_write_fnidx = if needs_print {
        Some(user_write_fnidx.unwrap_or(FnIdx(real_fn_count as u32)))
    } else {
        None
    };
    let inject_synthetic_write = needs_print && user_write_fnidx.is_none();

    let mut functions = Vec::new();
    let mut string_literals: Vec<Vec<u8>> = Vec::new();
    for def in &resolved.defs {
        match def.kind {
            DefKind::Fn => {
                let fn_decl = FnDecl::cast(def.syntax).expect("DefKind::Fn syntax must be FnDecl");
                let sig = typed.sigs.get(&def.id).cloned().unwrap_or(FnSig {
                    params: Vec::new(),
                    ret: Ty::Error,
                });
                let mir_fn = lower_fn(
                    def.name.clone(),
                    fn_decl,
                    &sig,
                    typed,
                    sm,
                    &def_to_fn,
                    &mut string_literals,
                    print_write_fnidx,
                );
                functions.push(mir_fn);
            }
            DefKind::SyntheticMain => {
                let module =
                    Module::cast(def.syntax).expect("DefKind::SyntheticMain syntax must be Module");
                let mir_fn = lower_synthetic_main(
                    module,
                    typed,
                    sm,
                    &def_to_fn,
                    &mut string_literals,
                    print_write_fnidx,
                );
                functions.push(mir_fn);
            }
            DefKind::Class => {
                // Classes have no MIR function.
            }
        }
    }
    if inject_synthetic_write {
        functions.push(synthesise_write_extern());
    }
    MirProgram {
        functions,
        class_layouts: typed.classes.clone(),
        string_literals,
    }
}

/// Synthesise an `extern fn write(fd: i32, buf: *u8, count: usize) -> isize;`
/// declaration. Phase 1 increment 11c uses this when the user did not
/// declare `write` themselves but the program contains a Print desugar.
/// The Cranelift backend lowers this with `Linkage::Import`, so the
/// system linker resolves the symbol against libc.
fn synthesise_write_extern() -> MirFn {
    MirFn {
        name: "write".to_string(),
        params: vec![Local(0), Local(1), Local(2)],
        return_ty: Ty::Int(IntTy::ISize),
        locals: vec![
            LocalDecl {
                ty: Ty::Int(IntTy::I32),
                span: Span::synthetic(),
            },
            LocalDecl {
                ty: Ty::Ptr(IntTy::U8),
                span: Span::synthetic(),
            },
            LocalDecl {
                ty: Ty::Int(IntTy::USize),
                span: Span::synthetic(),
            },
        ],
        blocks: Vec::new(),
        is_extern: true,
    }
}

/// Check whether the user declared a top-level `extern fn write` we can
/// reuse for Print desugaring. Phase 1 trusts the user-declared
/// signature; if it conflicts with `(i32, *u8, usize) -> isize` codegen
/// will report the duplicate-declare error.
fn find_user_write_extern(
    resolved: &ResolvedModule<'_>,
    def_to_fn: &FxHashMap<DefId, FnIdx>,
) -> Option<FnIdx> {
    let def = resolved.lookup("write")?;
    if def.kind != DefKind::Fn {
        return None;
    }
    let fn_decl = FnDecl::cast(def.syntax)?;
    if !fn_decl.is_extern() {
        return None;
    }
    def_to_fn.get(&def.id).copied()
}

/// Pre-scan the resolved module for any statement-position string
/// literal. Returns `true` as soon as one is found anywhere reachable
/// from a function body or the synthetic main's top-level stmts. The
/// scan recurses through `if`/`while`/`for`/block bodies.
fn module_needs_print_desugar(resolved: &ResolvedModule<'_>) -> bool {
    for def in &resolved.defs {
        match def.kind {
            DefKind::Fn => {
                if let Some(fn_decl) = FnDecl::cast(def.syntax) {
                    if let Some(body) = fn_decl.body() {
                        if block_contains_print_stmt(body) {
                            return true;
                        }
                    }
                }
            }
            DefKind::SyntheticMain => {
                if let Some(module) = Module::cast(def.syntax) {
                    for stmt in module.stmts() {
                        if stmt_contains_print(stmt) {
                            return true;
                        }
                    }
                }
            }
            DefKind::Class => {}
        }
    }
    false
}

fn block_contains_print_stmt<'a>(block: Block<'a>) -> bool {
    for stmt in block.stmts() {
        if stmt_contains_print(stmt) {
            return true;
        }
    }
    false
}

fn stmt_contains_print<'a>(stmt: Stmt<'a>) -> bool {
    match stmt {
        Stmt::Let(l) => l.init().is_some_and(expr_contains_print_stmt),
        Stmt::Expr(es) => {
            let Some(expr) = es.expr() else {
                return false;
            };
            if expr_is_string_literal(&expr) {
                return true;
            }
            expr_contains_print_stmt(expr)
        }
        Stmt::Stub(_) | Stmt::Error(_) => false,
    }
}

fn expr_is_string_literal<'a>(expr: &Expr<'a>) -> bool {
    let Expr::Literal(lit) = expr else {
        return false;
    };
    matches!(
        lit.token_kind(),
        Some(SyntaxKind::StringLit | SyntaxKind::RawStringLit)
    )
}

fn expr_contains_print_stmt<'a>(expr: Expr<'a>) -> bool {
    match expr {
        Expr::If(i) => {
            i.then_block().is_some_and(block_contains_print_stmt)
                || i.else_branch().is_some_and(expr_contains_print_stmt)
        }
        Expr::While(w) => w.body().is_some_and(block_contains_print_stmt),
        Expr::For(fe) => fe.body().is_some_and(block_contains_print_stmt),
        Expr::Block(b) => block_contains_print_stmt(b),
        // Other expression forms don't contain statement-position
        // contexts. Bare string literals at expression position (e.g.
        // `let s = "hi"`) are *not* desugared — only `"hi";` at a
        // statement boundary is.
        _ => false,
    }
}

/// Lower the synthetic `main` produced by Phase 1 increment 11a from
/// top-level statements. The signature is `() -> i32`; if control falls
/// off the end of the body without an explicit `return`, an implicit
/// `Return(Const::Int { value: 0, ty: I32 })` is appended so the program
/// exits 0 by default.
#[allow(clippy::too_many_arguments)]
fn lower_synthetic_main<'a>(
    module: Module<'a>,
    typed: &TypedModule<'a>,
    sm: &SourceMap,
    def_to_fn: &FxHashMap<DefId, FnIdx>,
    string_literals: &mut Vec<Vec<u8>>,
    print_write_fnidx: Option<FnIdx>,
) -> MirFn {
    let mut b = Builder::new();
    let entry = b.alloc_block();
    b.cur = entry;

    let mut lcx = LowerCx {
        typed,
        sm,
        def_to_fn,
        binding_to_local: FxHashMap::default(),
        string_literals,
        print_write_fnidx,
    };
    for stmt in module.stmts() {
        lower_stmt(&mut b, stmt, &mut lcx);
    }

    if !b.blocks[b.cur.0 as usize].terminator_set {
        b.set_terminator(
            b.cur,
            Terminator::Return(Operand::Const(Const::Int {
                value: 0,
                ty: IntTy::I32,
            })),
        );
    }

    MirFn {
        name: "main".to_string(),
        params: Vec::new(),
        return_ty: Ty::Int(IntTy::I32),
        locals: b.locals,
        blocks: b.blocks.into_iter().map(|x| x.into_block()).collect(),
        is_extern: false,
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_fn<'a>(
    name: String,
    fn_decl: FnDecl<'a>,
    sig: &FnSig,
    typed: &TypedModule<'a>,
    sm: &SourceMap,
    def_to_fn: &FxHashMap<DefId, FnIdx>,
    string_literals: &mut Vec<Vec<u8>>,
    print_write_fnidx: Option<FnIdx>,
) -> MirFn {
    let mut b = Builder::new();

    // Allocate parameter locals first so their indices are 0..params.
    // Look up each parameter's BindingId from the typed module's
    // `pat_bindings` map keyed on the AST `Param` node — typeck and
    // MIR allocate "binding-like things" at different paces (MIR
    // introduces fresh `Local`s for expression intermediates that
    // typeck never sees), so we must not infer the BindingId from
    // MIR's `local.0`.
    let mut binding_to_local: FxHashMap<BindingId, Local> = FxHashMap::default();
    let ast_params: Vec<_> = fn_decl
        .params()
        .map(|pl| pl.params().collect::<Vec<_>>())
        .unwrap_or_default();
    for (sig_p, ast_p) in sig.params.iter().zip(ast_params.iter()) {
        let local = b.alloc_local(LocalDecl {
            ty: sig_p.ty,
            span: sig_p.name_span,
        });
        b.params.push(local);
        let binding_id = typed
            .pat_bindings
            .get(&NodePtr(ast_p.syntax()))
            .copied()
            .unwrap_or(BindingId(local.0));
        binding_to_local.insert(binding_id, local);
    }

    // Extern declarations have no body in this translation unit; we
    // emit them as block-less MirFns and let codegen declare them with
    // import-style linkage so the system linker resolves them against
    // libc / another object.
    let is_extern = fn_decl.body().is_none();

    if !is_extern {
        // Open the entry block.
        let entry = b.alloc_block();
        b.cur = entry;

        if let Some(body) = fn_decl.body() {
            let mut lcx = LowerCx {
                typed,
                sm,
                def_to_fn,
                binding_to_local,
                string_literals,
                print_write_fnidx,
            };
            let _ = lower_block(&mut b, body, &mut lcx);
        }

        // If the trailing block has no terminator yet, fabricate one.
        // This happens when the function body falls through without an
        // explicit `return`. For a function returning `u0` this is
        // fine; for any other return type it would have been a type-
        // checker error already, and the unreachable terminator is a
        // safety net.
        if b.blocks[b.cur.0 as usize].terminator_set {
            // already set
        } else if sig.ret == Ty::U0 || sig.ret == Ty::Error {
            b.set_terminator(b.cur, Terminator::Return(Operand::Const(Const::Unit)));
        } else {
            b.set_terminator(b.cur, Terminator::Unreachable);
        }
    }

    MirFn {
        name,
        params: b.params,
        return_ty: sig.ret,
        locals: b.locals,
        blocks: b.blocks.into_iter().map(|x| x.into_block()).collect(),
        is_extern,
    }
}

// ─── builder ──────────────────────────────────────────────────────────

struct Builder {
    params: Vec<Local>,
    locals: Vec<LocalDecl>,
    blocks: Vec<DraftBlock>,
    /// Currently-open block. New statements go into this block.
    cur: BlockId,
    /// Stack of (continue_bb, break_bb) pairs for the lexically
    /// enclosing loops. Pushed on entering a `while`/`for` body and
    /// popped on exit; consumed by `lower_break` / `lower_continue`.
    loop_targets: Vec<LoopTarget>,
}

/// Where `break` and `continue` jump within the current loop.
#[derive(Copy, Clone, Debug)]
struct LoopTarget {
    /// Block to jump to when `continue` is encountered (loop header).
    continue_bb: BlockId,
    /// Block to jump to when `break` is encountered (loop exit).
    break_bb: BlockId,
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
            loop_targets: Vec::new(),
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

struct LowerCx<'a, 'tm, 'sm, 'm, 'sl> {
    typed: &'tm TypedModule<'a>,
    sm: &'sm SourceMap,
    def_to_fn: &'m FxHashMap<DefId, FnIdx>,
    binding_to_local: FxHashMap<BindingId, Local>,
    /// Program-level string-literal bytes table; lowering appends here
    /// when it sees a string literal expression and uses the resulting
    /// index as the [`StringLitId`].
    string_literals: &'sl mut Vec<Vec<u8>>,
    /// `FnIdx` of the `write` extern declaration to use for Phase 1
    /// implicit Print desugaring. `None` if the program contains no
    /// statement-position string literals (so `write` was not injected).
    print_write_fnidx: Option<FnIdx>,
}

fn lower_block<'a>(
    b: &mut Builder,
    block: Block<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
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

fn lower_stmt<'a>(b: &mut Builder, stmt: Stmt<'a>, lcx: &mut LowerCx<'a, '_, '_, '_, '_>) {
    match stmt {
        Stmt::Let(l) => lower_let(b, l, lcx),
        Stmt::Expr(es) => lower_expr_stmt(b, es, lcx),
        Stmt::Stub(_) | Stmt::Error(_) => {}
    }
}

fn lower_let<'a>(b: &mut Builder, let_stmt: LetStmt<'a>, lcx: &mut LowerCx<'a, '_, '_, '_, '_>) {
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
            // Allocate the binding's Local *before* lowering the init,
            // so init expressions allocate fresh higher-numbered Locals
            // for any temps and don't displace this binding's slot.
            let local = b.alloc_local(LocalDecl { ty, span });
            // Look up the BindingId typeck assigned to this pattern.
            // Falling back to `BindingId(local.0)` only matters for
            // bodies where typeck didn't run (which doesn't happen in
            // production lowering — fail soft rather than panic).
            let binding_id = lcx
                .typed
                .pat_bindings
                .get(&NodePtr(p.syntax()))
                .copied()
                .unwrap_or(BindingId(local.0));
            lcx.binding_to_local.insert(binding_id, local);
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

fn lower_expr_stmt<'a>(b: &mut Builder, es: ExprStmt<'a>, lcx: &mut LowerCx<'a, '_, '_, '_, '_>) {
    let Some(e) = es.expr() else {
        return;
    };
    // Phase 1 increment 11c: a bare string-literal at statement
    // position desugars to an implicit `Print` (spec §5.15.1). We
    // recognise the shape here and emit the `write(1, str.data,
    // str.len)` call inline.
    if let Expr::Literal(lit) = e {
        if matches!(
            lit.token_kind(),
            Some(SyntaxKind::StringLit | SyntaxKind::RawStringLit)
        ) {
            lower_implicit_print(b, lit, lcx);
            return;
        }
    }
    let _ = lower_expr(b, e, lcx);
}

/// Lower a statement-position string literal as `write(1, slice.data,
/// slice.len)` (Phase 1 increment 11c). The literal is first lowered
/// the usual way to materialise a slice-typed temp local, then we
/// extract its `data` and `len` fields into primitive locals and emit
/// a `Terminator::Call` against the program's chosen `write` fn idx.
/// The call's return value (an `isize`, the byte count actually
/// written) is captured into a discarded local since spec §5.15.1's
/// Print is statement-positioned and produces no value.
fn lower_implicit_print<'a>(
    b: &mut Builder,
    lit: LiteralExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) {
    // If the pre-scan never registered a write fn idx, the desugar
    // can't run. This can happen if the scan and lowering disagree
    // about whether a string-stmt exists (it shouldn't); fall back to
    // dropping the literal silently rather than producing a bad call.
    let Some(write_fnidx) = lcx.print_write_fnidx else {
        let _ = lower_literal(b, lit, lcx);
        return;
    };

    // Step 1: lower the literal as a normal `[]u8` slice (allocates a
    // temp slice local, populates the rodata table, and emits the two
    // AssignField stmts setting `data` and `len`).
    let slice_op = lower_literal(b, lit, lcx);
    let Operand::Local(slice_local) = slice_op else {
        return;
    };

    let span = lit.syntax().span;

    // Step 2: extract `slice.data` into a `*u8` primitive local.
    let data_local = b.alloc_local(LocalDecl {
        ty: Ty::Ptr(IntTy::U8),
        span,
    });
    b.push_stmt(MirStmt::Assign {
        dst: data_local,
        value: Rvalue::Field {
            base: slice_local,
            field_idx: 0,
            field_ty: Ty::Ptr(IntTy::U8),
        },
    });

    // Step 3: extract `slice.len` into a `usize` primitive local.
    let len_local = b.alloc_local(LocalDecl {
        ty: Ty::Int(IntTy::USize),
        span,
    });
    b.push_stmt(MirStmt::Assign {
        dst: len_local,
        value: Rvalue::Field {
            base: slice_local,
            field_idx: 1,
            field_ty: Ty::Int(IntTy::USize),
        },
    });

    // Step 4: discardable destination for `write`'s `isize` return.
    let ret_local = b.alloc_local(LocalDecl {
        ty: Ty::Int(IntTy::ISize),
        span,
    });

    // Step 5: emit the call and continue lowering in a fresh block.
    let cont = b.alloc_block();
    b.set_terminator(
        b.cur,
        Terminator::Call {
            callee: write_fnidx,
            args: vec![
                Operand::Const(Const::Int {
                    value: 1, // STDOUT_FILENO
                    ty: IntTy::I32,
                }),
                Operand::Local(data_local),
                Operand::Local(len_local),
            ],
            dst: ret_local,
            target_bb: cont,
        },
    );
    b.cur = cont;
}

fn lower_expr<'a>(
    b: &mut Builder,
    expr: Expr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
    match expr {
        Expr::Literal(l) => lower_literal(b, l, lcx),
        Expr::Path(p) => lower_path(p, lcx),
        Expr::Paren(p) => lower_paren(b, p, lcx),
        Expr::Binary(bin) => match bin.op_kind() {
            Some(SyntaxKind::Eq) => lower_assign(b, bin, lcx),
            Some(SyntaxKind::AmpAmp | SyntaxKind::PipePipe) => lower_short_circuit(b, bin, lcx),
            _ => lower_binary(b, bin, lcx),
        },
        Expr::Unary(u) => lower_unary(b, u, lcx),
        Expr::Block(blk) => lower_block(b, blk, lcx),
        Expr::If(i) => lower_if(b, i, lcx),
        Expr::While(w) => lower_while(b, w, lcx),
        Expr::Return(r) => lower_return(b, r, lcx),
        Expr::Call(c) => lower_call(b, c, lcx),
        Expr::Break(_) => lower_break(b),
        Expr::Continue(_) => lower_continue(b),
        Expr::For(fe) => lower_for(b, fe, lcx),
        Expr::StructLit(s) => lower_struct_lit(b, s, lcx),
        Expr::Field(fe) => lower_field(b, fe, lcx),
        Expr::Cast(c) => lower_cast(b, c, lcx),
        Expr::Stub(_) | Expr::Error(_) => Operand::Const(Const::Error),
    }
}

fn lower_literal<'a>(
    b: &mut Builder,
    l: LiteralExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
    let ty = lcx
        .typed
        .expr_types
        .get(&NodePtr(l.syntax()))
        .copied()
        .unwrap_or(Ty::Error);
    // Slice the underlying TOKEN's span, not the LiteralExpr node's
    // span — node spans extend to the next significant token, which
    // would pick up trailing trivia and break numeric parsing.
    let (kind, raw) = match l.token() {
        Some((k, span)) => (Some(k), lcx.sm.slice(span).unwrap_or("")),
        None => (None, ""),
    };
    match kind {
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
        Some(SyntaxKind::StringLit | SyntaxKind::RawStringLit) => {
            lower_string_literal(b, l, raw, lcx)
        }
        // Runes, byte chars — Phase 1 doesn't yet handle.
        _ => Operand::Const(Const::Error),
    }
}

/// Lower a `"..."` string literal to a `[]u8` slice value. Allocates a
/// fresh slice-typed local and emits two `AssignField` statements:
/// `field_idx 0` (data ptr) ← `Const::DataAddr(id)`, and `field_idx 1`
/// (length, usize) ← the byte count of the decoded payload. The
/// underlying bytes are interned in `lcx.string_literals` so codegen
/// can declare exactly one `.rodata` data object per occurrence.
fn lower_string_literal<'a>(
    b: &mut Builder,
    l: LiteralExpr<'a>,
    raw: &str,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
    let bytes = decode_string_literal(raw);
    let id = StringLitId(lcx.string_literals.len() as u32);
    let len = bytes.len();
    lcx.string_literals.push(bytes);
    let span = l.syntax().span;
    let dst = b.alloc_local(LocalDecl {
        ty: Ty::Slice(IntTy::U8),
        span,
    });
    b.push_stmt(MirStmt::AssignField {
        dst,
        field_idx: 0,
        value: Rvalue::Use(Operand::Const(Const::DataAddr(id))),
    });
    b.push_stmt(MirStmt::AssignField {
        dst,
        field_idx: 1,
        value: Rvalue::Use(Operand::Const(Const::Int {
            value: len as i128,
            ty: IntTy::USize,
        })),
    });
    Operand::Local(dst)
}

/// Decode a `"..."` string literal token into its raw bytes. Strips the
/// surrounding double quotes the lexer leaves on the token text and
/// processes the small set of Phase-1-supported escape sequences:
/// `\n`, `\t`, `\r`, `\0`, `\\`, `\"`, `\'`. Unknown escapes pass
/// through literally (the leading backslash + the following byte).
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

fn lower_path<'a>(p: PathExpr<'a>, lcx: &mut LowerCx<'a, '_, '_, '_, '_>) -> Operand {
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
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
    p.inner()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Error))
}

/// Lower `lhs = rhs`. The LHS must be a path to a local (typeck has
/// already enforced this and pushed a `BAD_OPERAND` diagnostic
/// otherwise). Yields `Operand::Const(Const::Unit)` since assignment
/// is a `u0`-typed expression.
fn lower_assign<'a>(
    b: &mut Builder,
    bin: BinaryExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
    let rhs_val = bin
        .rhs()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Error));
    match bin.lhs() {
        Some(Expr::Path(p)) => {
            let dst = lcx
                .typed
                .path_bindings
                .get(&NodePtr(p.syntax()))
                .and_then(|bid| lcx.binding_to_local.get(bid))
                .copied();
            if let Some(local) = dst {
                b.push_stmt(MirStmt::Assign {
                    dst: local,
                    value: Rvalue::Use(rhs_val),
                });
            }
        }
        Some(Expr::Field(fe)) => {
            // `local.field = rhs`. Resolve the local from the base
            // path and the field index from typeck's class layout.
            if let Some((base_local, field_idx)) = resolve_field_lvalue(fe, lcx) {
                b.push_stmt(MirStmt::AssignField {
                    dst: base_local,
                    field_idx,
                    value: Rvalue::Use(rhs_val),
                });
            }
        }
        _ => {}
    }
    Operand::Const(Const::Unit)
}

/// Resolve a `base.field` field-access expression in lvalue position.
/// Returns `(base_local, field_idx)` if the base is a path to a
/// class-typed local and the field is declared by that class.
fn resolve_field_lvalue<'a>(
    fe: FieldExpr<'a>,
    lcx: &LowerCx<'a, '_, '_, '_, '_>,
) -> Option<(Local, u32)> {
    let base = fe.base()?;
    let base_local = match base {
        Expr::Path(p) => {
            let bid = lcx.typed.path_bindings.get(&NodePtr(p.syntax()))?;
            *lcx.binding_to_local.get(bid)?
        }
        _ => return None,
    };
    // Slice bases use a hardcoded 2-field layout: data at index 0,
    // len at index 1. Matches the synthetic layout codegen materialises.
    if let Some(Ty::Slice(_)) = lcx.typed.expr_types.get(&NodePtr(base.syntax())).copied() {
        let name_span = fe.field_name()?;
        let name = lcx.sm.slice(name_span)?;
        let idx = match name {
            "data" => 0,
            "len" => 1,
            _ => return None,
        };
        return Some((base_local, idx));
    }
    // Look up class via the base's expr type (typeck recorded it).
    let class_id = match lcx.typed.expr_types.get(&NodePtr(base.syntax())).copied()? {
        Ty::Class(id) => id,
        _ => return None,
    };
    let layout = lcx.typed.classes.get(&class_id)?;
    let name_span = fe.field_name()?;
    let name = lcx.sm.slice(name_span)?;
    let idx = layout.fields.iter().position(|f| f.name == name)?;
    Some((base_local, idx as u32))
}

fn lower_binary<'a>(
    b: &mut Builder,
    bin: BinaryExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
    // Capture each operand's type *before* lowering: the codegen needs
    // the operand width, which differs from the result width for
    // comparison operators (operands i32, result bool).
    let operand_ty = bin
        .lhs()
        .and_then(|e| lcx.typed.expr_types.get(&NodePtr(e.syntax())).copied())
        .unwrap_or(Ty::Error);
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
    // `Rvalue::BinOp::ty` carries the OPERAND type (which equals the
    // result type for arithmetic but differs for comparison). Codegen
    // uses it to decide operand width, signed-vs-unsigned ops, and
    // float-vs-int dispatch.
    let value = Rvalue::BinOp {
        op,
        lhs,
        rhs,
        ty: operand_ty,
    };
    let local = b.alloc_local(LocalDecl {
        ty: result_ty,
        span: bin.syntax().span,
    });
    b.push_stmt(MirStmt::Assign { dst: local, value });
    Operand::Local(local)
}

/// Lower `lhs && rhs` and `lhs || rhs` with short-circuit semantics.
///
/// Both operators desugar to control flow: the RHS is only evaluated
/// when the LHS does not already determine the result.
///
/// Shape (for `&&`; `||` swaps the then/else targets):
/// ```text
///     branch on lhs:
///       true  -> rhs_bb
///       false -> short_bb (assigns false, gotos join)
///     rhs_bb: <evaluate rhs>; result = rhs; goto join
///     short_bb: result = false; goto join
///     join: continue with `Operand::Local(result)`
/// ```
fn lower_short_circuit<'a>(
    b: &mut Builder,
    bin: BinaryExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
    let is_and = matches!(bin.op_kind(), Some(SyntaxKind::AmpAmp));

    // The result local lives in whatever block we came from; it is
    // assigned in both the rhs-eval block and the short-circuit block,
    // and read at the join.
    let result = b.alloc_local(LocalDecl {
        ty: Ty::Bool,
        span: bin.syntax().span,
    });

    let lhs = bin
        .lhs()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Bool(!is_and)));

    let rhs_bb = b.alloc_block();
    let short_bb = b.alloc_block();
    let join_bb = b.alloc_block();

    // For `&&`: take rhs_bb when lhs is true, short_bb when false.
    // For `||`: take short_bb when lhs is true, rhs_bb when false.
    let (then_bb, else_bb) = if is_and {
        (rhs_bb, short_bb)
    } else {
        (short_bb, rhs_bb)
    };
    b.set_terminator(
        b.cur,
        Terminator::Branch {
            cond: lhs,
            then_bb,
            else_bb,
        },
    );

    // Short-circuit block: result is `false` for `&&`, `true` for `||`.
    b.cur = short_bb;
    b.push_stmt(MirStmt::Assign {
        dst: result,
        value: Rvalue::Use(Operand::Const(Const::Bool(!is_and))),
    });
    b.set_terminator(b.cur, Terminator::Goto(join_bb));

    // RHS-eval block.
    b.cur = rhs_bb;
    let rhs = bin
        .rhs()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Bool(is_and)));
    // `lower_expr` for the rhs may have introduced its own blocks
    // (e.g. nested `&&`); use the builder's current block, not rhs_bb.
    b.push_stmt(MirStmt::Assign {
        dst: result,
        value: Rvalue::Use(rhs),
    });
    b.set_terminator(b.cur, Terminator::Goto(join_bb));

    b.cur = join_bb;
    Operand::Local(result)
}

fn lower_unary<'a>(
    b: &mut Builder,
    u: UnaryExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
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

/// Lower `expr as Type` to a [`Rvalue::Cast`].
///
/// The [`CastKind`] is selected here from operand and target types;
/// codegen then maps `kind` to a single Cranelift op. Phase 1
/// increments A.1 / A.2 cover the full numeric matrix (int↔int,
/// int↔float, float↔float). Non-numeric source or destination is
/// already diagnosed by typeck; we fall through to `Const::Error` so
/// codegen has a defused operand.
fn lower_cast<'a>(
    b: &mut Builder,
    c: CastExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
    let operand = c
        .expr()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Error));
    let src_ty = c
        .expr()
        .and_then(|e| lcx.typed.expr_types.get(&NodePtr(e.syntax())).copied())
        .unwrap_or(Ty::Error);
    let dst_ty = lcx
        .typed
        .expr_types
        .get(&NodePtr(c.syntax()))
        .copied()
        .unwrap_or(Ty::Error);
    let Some(kind) = select_cast_kind(src_ty, dst_ty) else {
        // Typeck diagnosed already; defuse so codegen stays sound.
        return Operand::Const(Const::Error);
    };
    let local = b.alloc_local(LocalDecl {
        ty: dst_ty,
        span: c.syntax().span,
    });
    b.push_stmt(MirStmt::Assign {
        dst: local,
        value: Rvalue::Cast {
            kind,
            operand,
            src_ty,
            dst_ty,
        },
    });
    Operand::Local(local)
}

/// Pick the [`CastKind`] for a numeric `src` → `dst` pair.
///
/// Returns `None` for non-numeric pairs (typeck has already diagnosed
/// these). Pointer width is fixed at 64 on every Phase-1 target,
/// matching `value_fits_int`'s bound.
fn select_cast_kind(src: Ty, dst: Ty) -> Option<CastKind> {
    const PTR_BITS: u32 = 64;
    Some(match (src, dst) {
        (Ty::Int(_), Ty::Int(_)) => {
            let s = src.int_bits(PTR_BITS)?;
            let d = dst.int_bits(PTR_BITS)?;
            match s.cmp(&d) {
                std::cmp::Ordering::Less => CastKind::IntWiden {
                    signed: src.is_signed_int(),
                },
                std::cmp::Ordering::Greater => CastKind::IntTrunc,
                std::cmp::Ordering::Equal => CastKind::IntBitcast,
            }
        }
        (Ty::Int(_), Ty::Float(_)) => CastKind::IntToFloat {
            signed: src.is_signed_int(),
        },
        (Ty::Float(_), Ty::Int(_)) => CastKind::FloatToInt {
            signed: dst.is_signed_int(),
        },
        (Ty::Float(s), Ty::Float(d)) => match (s, d) {
            (FloatTy::F32, FloatTy::F64) => CastKind::FloatExt,
            (FloatTy::F64, FloatTy::F32) => CastKind::FloatTrunc,
            (FloatTy::F32, FloatTy::F32) | (FloatTy::F64, FloatTy::F64) => CastKind::FloatBitcast,
        },
        _ => return None,
    })
}

fn lower_if<'a>(b: &mut Builder, i: IfExpr<'a>, lcx: &mut LowerCx<'a, '_, '_, '_, '_>) -> Operand {
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

    // Allocate a result local only for if-expressions whose value is
    // observed at runtime. A `Ty::U0` if (e.g. one whose arms both
    // diverge via `return`, or has no `else`) has no runtime
    // representation, and creating a Cranelift Variable for it would
    // force codegen to def_var an i32-shaped Unit constant into an
    // i8-typed Variable — a verifier failure.
    let result_local = if !matches!(result_ty, Ty::U0 | Ty::Error) {
        Some(b.alloc_local(LocalDecl {
            ty: result_ty,
            span: i.syntax().span,
        }))
    } else {
        None
    };

    // Then arm.
    b.cur = then_bb;
    let then_val = i
        .then_block()
        .map(|blk| lower_block(b, blk, lcx))
        .unwrap_or(Operand::Const(Const::Unit));
    if let Some(local) = result_local {
        b.push_stmt(MirStmt::Assign {
            dst: local,
            value: Rvalue::Use(then_val),
        });
    }
    b.set_terminator(b.cur, Terminator::Goto(join_bb));

    // Else arm.
    b.cur = else_bb;
    let else_val = match i.else_branch() {
        Some(branch) => lower_expr(b, branch, lcx),
        None => Operand::Const(Const::Unit),
    };
    if let Some(local) = result_local {
        b.push_stmt(MirStmt::Assign {
            dst: local,
            value: Rvalue::Use(else_val),
        });
    }
    b.set_terminator(b.cur, Terminator::Goto(join_bb));

    // Continue lowering at the join block.
    b.cur = join_bb;
    match result_local {
        Some(l) => Operand::Local(l),
        None => Operand::Const(Const::Unit),
    }
}

fn lower_while<'a>(
    b: &mut Builder,
    w: WhileExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
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

    // Body. Push the loop targets so any `break` or `continue` inside
    // resolves to the right blocks; `continue` re-enters the header to
    // re-evaluate the condition.
    b.cur = body_bb;
    b.loop_targets.push(LoopTarget {
        continue_bb: header_bb,
        break_bb: exit_bb,
    });
    if let Some(body) = w.body() {
        let _ = lower_block(b, body, lcx);
    }
    b.loop_targets.pop();
    b.set_terminator(b.cur, Terminator::Goto(header_bb));

    // Continue at exit.
    b.cur = exit_bb;
    Operand::Const(Const::Unit)
}

/// Lower `break`. Jumps to the enclosing loop's exit block, then opens
/// a fresh dead block to absorb any further statements in the same
/// source block (those won't be reached at runtime). Outside any loop,
/// emits no terminator — typeck has already pushed a diagnostic.
fn lower_break(b: &mut Builder) -> Operand {
    if let Some(target) = b.loop_targets.last().copied() {
        b.set_terminator(b.cur, Terminator::Goto(target.break_bb));
        let dead = b.alloc_block();
        b.cur = dead;
    }
    Operand::Const(Const::Unit)
}

/// Lower `continue`. Jumps to the enclosing loop's continue target
/// (the header for `while`, the increment block for `for`).
fn lower_continue(b: &mut Builder) -> Operand {
    if let Some(target) = b.loop_targets.last().copied() {
        b.set_terminator(b.cur, Terminator::Goto(target.continue_bb));
        let dead = b.alloc_block();
        b.cur = dead;
    }
    Operand::Const(Const::Unit)
}

/// Lower `for x in lo..hi { body }` by desugaring into:
///
/// ```text
///     let __counter = lo;
///     let __end     = hi;
/// header:
///     if __counter >= __end goto exit    (or > for `..=`)
///     x = __counter
///     body
/// step:
///     __counter = __counter + 1
///     goto header
/// exit:
/// ```
///
/// Continues jump to `step`, breaks jump to `exit`.
fn lower_for<'a>(
    b: &mut Builder,
    fe: ForExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
    // Determine the iteration type from the start bound's typeck record.
    let iter_ty = fe
        .range_start()
        .and_then(|e| lcx.typed.expr_types.get(&NodePtr(e.syntax())).copied())
        .unwrap_or(Ty::Error);
    let inclusive = fe.inclusive();

    // Lower the bounds *before* opening the loop blocks so any nested
    // expressions emit into the predecessor.
    let start_val = fe
        .range_start()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Error));
    let end_val = fe
        .range_end()
        .map(|e| lower_expr(b, e, lcx))
        .unwrap_or(Operand::Const(Const::Error));

    let span = fe.syntax().span;
    let counter = b.alloc_local(LocalDecl { ty: iter_ty, span });
    let end_local = b.alloc_local(LocalDecl { ty: iter_ty, span });

    // Initialise counter and end.
    b.push_stmt(MirStmt::Assign {
        dst: counter,
        value: Rvalue::Use(start_val),
    });
    b.push_stmt(MirStmt::Assign {
        dst: end_local,
        value: Rvalue::Use(end_val),
    });

    // Allocate the loop blocks.
    let header_bb = b.alloc_block();
    let body_bb = b.alloc_block();
    let step_bb = b.alloc_block();
    let exit_bb = b.alloc_block();

    b.set_terminator(b.cur, Terminator::Goto(header_bb));

    // Header: cmp counter against end and branch.
    b.cur = header_bb;
    let cmp_op = if inclusive { BinOp::Le } else { BinOp::Lt };
    let cond_local = b.alloc_local(LocalDecl { ty: Ty::Bool, span });
    b.push_stmt(MirStmt::Assign {
        dst: cond_local,
        value: Rvalue::BinOp {
            op: cmp_op,
            lhs: Operand::Local(counter),
            rhs: Operand::Local(end_local),
            ty: iter_ty,
        },
    });
    b.set_terminator(
        b.cur,
        Terminator::Branch {
            cond: Operand::Local(cond_local),
            then_bb: body_bb,
            else_bb: exit_bb,
        },
    );

    // Body. Bind the user's loop variable to the counter's current value.
    b.cur = body_bb;
    if let Some(Pattern::Ident(p)) = fe.pattern() {
        let var_local = b.alloc_local(LocalDecl { ty: iter_ty, span });
        let bid = lcx
            .typed
            .pat_bindings
            .get(&NodePtr(p.syntax()))
            .copied()
            .unwrap_or(BindingId(var_local.0));
        lcx.binding_to_local.insert(bid, var_local);
        b.push_stmt(MirStmt::Assign {
            dst: var_local,
            value: Rvalue::Use(Operand::Local(counter)),
        });
    }
    // Wildcard pattern: nothing to bind.

    b.loop_targets.push(LoopTarget {
        continue_bb: step_bb,
        break_bb: exit_bb,
    });
    if let Some(body) = fe.body() {
        let _ = lower_block(b, body, lcx);
    }
    b.loop_targets.pop();
    b.set_terminator(b.cur, Terminator::Goto(step_bb));

    // Step: counter += 1.
    b.cur = step_bb;
    let one = match iter_ty {
        Ty::Int(int_ty) => Operand::Const(Const::Int {
            value: 1,
            ty: int_ty,
        }),
        _ => Operand::Const(Const::Int {
            value: 1,
            ty: IntTy::I32,
        }),
    };
    let next_local = b.alloc_local(LocalDecl { ty: iter_ty, span });
    b.push_stmt(MirStmt::Assign {
        dst: next_local,
        value: Rvalue::BinOp {
            op: BinOp::Add,
            lhs: Operand::Local(counter),
            rhs: one,
            ty: iter_ty,
        },
    });
    b.push_stmt(MirStmt::Assign {
        dst: counter,
        value: Rvalue::Use(Operand::Local(next_local)),
    });
    b.set_terminator(b.cur, Terminator::Goto(header_bb));

    // Continue at exit.
    b.cur = exit_bb;
    Operand::Const(Const::Unit)
}

fn lower_return<'a>(
    b: &mut Builder,
    r: ReturnExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
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

fn lower_call<'a>(
    b: &mut Builder,
    c: CallExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
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

/// Lower `Foo { .x = 1, .y = 2 }`. Allocates a fresh class-typed
/// local, emits one `AssignField` per provided field initialiser, and
/// returns `Operand::Local(...)` referring to the new local.
fn lower_struct_lit<'a>(
    b: &mut Builder,
    s: StructLitExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
    let class_ty = lcx
        .typed
        .expr_types
        .get(&NodePtr(s.syntax()))
        .copied()
        .unwrap_or(Ty::Error);
    let class_id = match class_ty {
        Ty::Class(id) => id,
        _ => return Operand::Const(Const::Error),
    };
    let layout = match lcx.typed.classes.get(&class_id) {
        Some(l) => l.clone(),
        None => return Operand::Const(Const::Error),
    };
    let dst = b.alloc_local(LocalDecl {
        ty: class_ty,
        span: s.syntax().span,
    });
    if let Some(list) = s.fields() {
        for fld in list.fields() {
            let value = match fld.value() {
                Some(e) => lower_expr(b, e, lcx),
                None => Operand::Const(Const::Error),
            };
            let name_span = match fld.name() {
                Some(sp) => sp,
                None => continue,
            };
            let name = match lcx.sm.slice(name_span) {
                Some(n) => n,
                None => continue,
            };
            if let Some(idx) = layout.fields.iter().position(|f| f.name == name) {
                b.push_stmt(MirStmt::AssignField {
                    dst,
                    field_idx: idx as u32,
                    value: Rvalue::Use(value),
                });
            }
        }
    }
    Operand::Local(dst)
}

/// Lower `base.field`. Phase 1 only supports direct `local.field`;
/// nested projections (`a.b.c`) require the user to bind intermediates
/// to locals first.
fn lower_field<'a>(
    b: &mut Builder,
    fe: FieldExpr<'a>,
    lcx: &mut LowerCx<'a, '_, '_, '_, '_>,
) -> Operand {
    let Some((base_local, field_idx)) = resolve_field_lvalue(fe, lcx) else {
        // Best-effort: still lower the base so any side effects fire.
        if let Some(base) = fe.base() {
            let _ = lower_expr(b, base, lcx);
        }
        return Operand::Const(Const::Error);
    };
    let field_ty = lcx
        .typed
        .expr_types
        .get(&NodePtr(fe.syntax()))
        .copied()
        .unwrap_or(Ty::Error);
    let dst = b.alloc_local(LocalDecl {
        ty: field_ty,
        span: fe.syntax().span,
    });
    b.push_stmt(MirStmt::Assign {
        dst,
        value: Rvalue::Field {
            base: base_local,
            field_idx,
            field_ty,
        },
    });
    Operand::Local(dst)
}

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

    /// Regression: when a `let` binding's initializer contains a
    /// binary expression, MIR allocates a fresh `Local` for the
    /// binop's intermediate, which advances the local counter past
    /// what typeck's BindingId allocator did. Earlier versions of
    /// `lower_let` registered `binding_to_local[BindingId(local.0)]`,
    /// inferring the BindingId from the just-allocated Local; this
    /// silently mismatched typeck's assignment and `return w;`
    /// resolved to a stale temp (often `Const::Error` → 0).
    ///
    /// Fix: typeck records `pat_bindings[NodePtr(IdentPat)] =
    /// BindingId`, MIR consults that map.
    #[test]
    fn let_binding_with_temped_init_resolves_to_correct_local() {
        let prog = lower_src(
            "fn add(x: i32, y: i32) -> i32 {
                let z: i32 = x + y;
                let w: i32 = z + 1;
                return w;
            }
            fn main() -> i32 { return add(2, 3); }",
        );
        let add_fn = prog
            .functions
            .iter()
            .find(|f| f.name == "add")
            .expect("add fn lowered");
        // The final block's terminator must be `Return(Local(_))`
        // pointing at a Local whose declaration matches `w` — i.e.,
        // the second-to-last let binding (since the last is `w` itself
        // after a temp). We don't inspect that exactly here; instead
        // assert the simpler invariant that the return is *not*
        // Return(Const::Error), which was the failure mode.
        let mut saw_real_return = false;
        for blk in &add_fn.blocks {
            if let Terminator::Return(Operand::Local(_)) = &blk.terminator {
                saw_real_return = true;
            }
            if let Terminator::Return(Operand::Const(Const::Error)) = &blk.terminator {
                panic!(
                    "regression: `return w` lowered to Const::Error, indicating BindingId mismatch"
                );
            }
        }
        assert!(
            saw_real_return,
            "expected at least one Return(Local) terminator in add()"
        );
    }

    /// Regression: a literal nested inside a binary expression must
    /// lower to its actual numeric value, not 0. Earlier versions of
    /// `lower_literal` sliced the LiteralExpr *node* span (which
    /// extends past trailing trivia) and parsed `"1 ".parse::<i128>()`
    /// — which fails — defaulting both operands to 0.
    #[test]
    fn binary_literal_operands_carry_correct_values() {
        let prog = lower_src("fn main() -> i32 { return 1 + 2; }");
        let entry = &prog.functions[0].blocks[0];
        let stmt = entry
            .statements
            .iter()
            .find_map(|s| match s {
                MirStmt::Assign {
                    value: Rvalue::BinOp { lhs, rhs, .. },
                    ..
                } => Some((lhs, rhs)),
                _ => None,
            })
            .expect("a BinOp Assign");
        match stmt {
            (
                Operand::Const(Const::Int { value: l, .. }),
                Operand::Const(Const::Int { value: r, .. }),
            ) => {
                assert_eq!(*l, 1);
                assert_eq!(*r, 2);
            }
            other => panic!("operands should both be Int constants, got {other:?}"),
        }
    }

    /// Regression: an `if`/`else if`/`else` chain must lower the
    /// nested `else if` as the outer if's `else_branch`, not silently
    /// drop it. The previous `IfExpr::else_branch` filtered for child
    /// IfExpr nodes and incorrectly skipped the first one (thinking
    /// it was `self`); since `child_nodes()` doesn't include `self`,
    /// it would skip the *actual* else-if and return None. Codegen
    /// then produced an unreachable code path that ran garbage and
    /// crashed via SIGILL at runtime.
    #[test]
    fn else_if_chain_branches_into_nested_if() {
        let prog = lower_src(
            "fn classify(x: i32) -> i32 {
                if x < 0 { return 1; }
                else if x < 10 { return 2; }
                else { return 3; }
            }
            fn main() -> i32 { return classify(5); }",
        );
        let classify = prog
            .functions
            .iter()
            .find(|f| f.name == "classify")
            .expect("classify lowered");
        // The entry block must terminate in a Branch; the else arm
        // (else_bb) must itself terminate in another Branch (the
        // nested else-if), not a plain Goto-into-default-return.
        let entry = &classify.blocks[0];
        let (then_bb, else_bb) = match &entry.terminator {
            Terminator::Branch {
                then_bb, else_bb, ..
            } => (*then_bb, *else_bb),
            other => panic!("entry should branch, got {other:?}"),
        };
        let _ = then_bb;
        let else_block = &classify.blocks[else_bb.0 as usize];
        assert!(
            matches!(else_block.terminator, Terminator::Branch { .. }),
            "else arm should itself branch on the nested if; got {:?}",
            else_block.terminator,
        );
    }

    /// Regression: `&&` and `||` lowered to `BinOp::LogAnd` /
    /// `BinOp::LogOr` (eager `band`/`bor` at codegen) instead of
    /// short-circuit control flow. With the bug, `false && side(c)`
    /// would still evaluate `side(c)`. The fix routes `AmpAmp` /
    /// `PipePipe` through `lower_short_circuit`, which emits a
    /// branch on the LHS, evaluates the RHS only on the take-branch,
    /// and joins both arms into a single result local.
    #[test]
    fn logical_and_or_lowers_as_branches_not_eager_binops() {
        let prog = lower_src(
            "fn f(a: bool, b: bool) -> bool { return a && b; }
             fn g(a: bool, b: bool) -> bool { return a || b; }
             fn main() -> i32 { return 0; }",
        );
        for name in ["f", "g"] {
            let func = prog
                .functions
                .iter()
                .find(|fn_| fn_.name == name)
                .expect("fn lowered");
            // Must contain at least one Branch terminator (the
            // short-circuit branch on the LHS).
            let has_branch = func
                .blocks
                .iter()
                .any(|blk| matches!(blk.terminator, Terminator::Branch { .. }));
            assert!(
                has_branch,
                "{name}: short-circuit should emit a Branch terminator"
            );
            // Must NOT contain a BinOp::LogAnd or BinOp::LogOr — those
            // are the eager-evaluation rvalues the bug produced.
            let has_log_binop = func.blocks.iter().any(|blk| {
                blk.statements.iter().any(|s| {
                    matches!(
                        s,
                        MirStmt::Assign {
                            value: Rvalue::BinOp {
                                op: BinOp::LogAnd | BinOp::LogOr,
                                ..
                            },
                            ..
                        }
                    )
                })
            });
            assert!(
                !has_log_binop,
                "{name}: short-circuit must not emit BinOp::LogAnd / LogOr"
            );
        }
    }

    // ─── Phase 1 increment A.1: `as` cast lowering ─────────────────────

    fn cast_kind_in(prog: &MirProgram, fn_name: &str) -> CastKind {
        let func = prog
            .functions
            .iter()
            .find(|f| f.name == fn_name)
            .expect("fn lowered");
        for blk in &func.blocks {
            for stmt in &blk.statements {
                if let MirStmt::Assign {
                    value: Rvalue::Cast { kind, .. },
                    ..
                } = stmt
                {
                    return *kind;
                }
            }
        }
        panic!("no Rvalue::Cast in {fn_name}");
    }

    /// Widening signed → wider chooses sign-extension based on the
    /// **operand**'s signedness, not the destination's.
    #[test]
    fn cast_signed_widen_picks_sextend_kind() {
        let prog = lower_src(
            "fn ext(x: i8) -> i64 { return x as i64; }
             fn main() -> i32 { return 0; }",
        );
        assert_eq!(
            cast_kind_in(&prog, "ext"),
            CastKind::IntWiden { signed: true }
        );
    }

    /// Widening unsigned → wider chooses zero-extension.
    #[test]
    fn cast_unsigned_widen_picks_uextend_kind() {
        let prog = lower_src(
            "fn ext(x: u8) -> u32 { return x as u32; }
             fn main() -> i32 { return 0; }",
        );
        assert_eq!(
            cast_kind_in(&prog, "ext"),
            CastKind::IntWiden { signed: false }
        );
    }

    /// Narrowing always picks `IntTrunc` regardless of signedness.
    #[test]
    fn cast_narrow_picks_trunc_kind() {
        let prog = lower_src(
            "fn nar(x: i64) -> i32 { return x as i32; }
             fn main() -> i32 { return 0; }",
        );
        assert_eq!(cast_kind_in(&prog, "nar"), CastKind::IntTrunc);
    }

    /// Same width different signedness → bit reinterpret, no codegen op.
    #[test]
    fn cast_same_width_signedness_picks_bitcast_kind() {
        let prog = lower_src(
            "fn rein(x: i32) -> u32 { return x as u32; }
             fn main() -> i32 { return 0; }",
        );
        assert_eq!(cast_kind_in(&prog, "rein"), CastKind::IntBitcast);
    }

    // ─── Phase 1 increment A.2: float bridge ───────────────────────────

    /// Signed int → float uses the operand's signedness.
    #[test]
    fn cast_signed_int_to_float_picks_signed_kind() {
        let prog = lower_src(
            "fn cv(x: i32) -> f64 { return x as f64; }
             fn main() -> i32 { return 0; }",
        );
        assert_eq!(
            cast_kind_in(&prog, "cv"),
            CastKind::IntToFloat { signed: true }
        );
    }

    /// Unsigned int → float uses the operand's signedness (false).
    #[test]
    fn cast_unsigned_int_to_float_picks_unsigned_kind() {
        let prog = lower_src(
            "fn cv(x: u32) -> f64 { return x as f64; }
             fn main() -> i32 { return 0; }",
        );
        assert_eq!(
            cast_kind_in(&prog, "cv"),
            CastKind::IntToFloat { signed: false }
        );
    }

    /// Float → signed int uses the destination's signedness.
    #[test]
    fn cast_float_to_signed_int_picks_signed_kind() {
        let prog = lower_src(
            "fn cv(x: f64) -> i32 { return x as i32; }
             fn main() -> i32 { return 0; }",
        );
        assert_eq!(
            cast_kind_in(&prog, "cv"),
            CastKind::FloatToInt { signed: true }
        );
    }

    /// Float → unsigned int uses the destination's signedness (false).
    #[test]
    fn cast_float_to_unsigned_int_picks_unsigned_kind() {
        let prog = lower_src(
            "fn cv(x: f64) -> u32 { return x as u32; }
             fn main() -> i32 { return 0; }",
        );
        assert_eq!(
            cast_kind_in(&prog, "cv"),
            CastKind::FloatToInt { signed: false }
        );
    }

    #[test]
    fn cast_f32_to_f64_picks_promote_kind() {
        let prog = lower_src(
            "fn cv(x: f32) -> f64 { return x as f64; }
             fn main() -> i32 { return 0; }",
        );
        assert_eq!(cast_kind_in(&prog, "cv"), CastKind::FloatExt);
    }

    #[test]
    fn cast_f64_to_f32_picks_demote_kind() {
        let prog = lower_src(
            "fn cv(x: f64) -> f32 { return x as f32; }
             fn main() -> i32 { return 0; }",
        );
        assert_eq!(cast_kind_in(&prog, "cv"), CastKind::FloatTrunc);
    }

    #[test]
    fn cast_same_float_width_picks_bitcast_kind() {
        let prog = lower_src(
            "fn id32(x: f32) -> f32 { return x as f32; }
             fn id64(x: f64) -> f64 { return x as f64; }
             fn main() -> i32 { return 0; }",
        );
        assert_eq!(cast_kind_in(&prog, "id32"), CastKind::FloatBitcast);
        assert_eq!(cast_kind_in(&prog, "id64"), CastKind::FloatBitcast);
    }
}
