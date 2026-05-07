//! GW LLVM backend (Phase 13).
//!
//! Mirrors the public contract of [`arsenal_codegen_fast::compile_program`]:
//! `MirProgram → object bytes`. The driver picks between the two backends
//! at `arsenal build --backend=fast|llvm`.
//!
//! Supported MIR subset (B.1 → B.3):
//! - integer + bool + float params and returns; `u0` returns
//! - integer + bool + float locals (alloca-backed; mem2reg promotion is
//!   left to LLVM's later opt-level when we add one)
//! - `Rvalue::Use` / `BinOp` / `UnOp` over integers, bools, and floats
//!   (float comparison via `OEQ`/`ONE`/`OLT`/etc., matching the
//!   Cranelift backend's "ordered, NaN→false" semantics)
//! - `Rvalue::Cast` across the full numeric matrix: int↔int (sext /
//!   zext / trunc / no-op), int↔float (sitofp / uitofp / saturating
//!   fptosi / fptoui via `llvm.fpto{si,ui}.sat` intrinsics), float↔float
//!   (fpext / fptrunc / no-op). Float→int matches Rust ≥ 1.45 / Cranelift
//!   `fcvt_to_*_sat`: saturating with NaN→0, no extra branch needed.
//! - `Operand::Const(Int|Bool|Float|Unit)`, `Operand::Local`
//! - `Terminator::{Goto, Branch, Return, Call, Unreachable}`
//!
//! Deferred to B.4+: classes, slices, `*T` raw pointers, string
//! literals + Print desugar, `Rvalue::Field` / `MirStmt::AssignField`.
//! Anything outside the supported set returns
//! [`CodegenError::Unsupported`] with a descriptive message.

use arsenal_mir::{
    BinOp, BlockId, CastKind, Const, FnIdx, Local, MirFn, MirProgram, MirStmt, Operand, Rvalue,
    Terminator, UnOp,
};
use arsenal_typeck::{FloatTy, IntTy, Ty};
use inkwell::basic_block::BasicBlock;
use inkwell::context::Context;
use inkwell::intrinsics::Intrinsic;
use inkwell::module::{Linkage, Module};
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetTriple,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, FloatType, FunctionType, IntType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FloatValue, FunctionValue, IntValue, PointerValue,
};
use inkwell::{FloatPredicate, IntPredicate, OptimizationLevel};
use rustc_hash::FxHashMap;
use target_lexicon::Triple;

/// Codegen error.
#[derive(Debug)]
pub enum CodegenError {
    /// Failed to resolve / initialise an LLVM target for the given triple.
    /// The contained string is the underlying error message.
    Target(String),
    /// LLVM IR builder error (e.g. malformed terminator).
    Builder(String),
    /// LLVM emit-to-object error.
    Emit(String),
    /// MIR construct not yet handled by the LLVM backend. Tracks the
    /// frontier as B.2–B.5 land; should be empty by B.5 / final corpus
    /// parity.
    Unsupported(String),
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Target(s) => write!(f, "llvm target setup failed: {s}"),
            Self::Builder(s) => write!(f, "llvm ir builder error: {s}"),
            Self::Emit(s) => write!(f, "llvm object emission failed: {s}"),
            Self::Unsupported(s) => write!(f, "llvm backend: unsupported construct: {s}"),
        }
    }
}

impl std::error::Error for CodegenError {}

/// Compile a [`MirProgram`] to an object file targeting `triple`.
///
/// Returns the raw bytes of the resulting object file (Mach-O / ELF /
/// COFF) — same caller contract as the Cranelift backend; the driver
/// hands the bytes to `cc` for linking.
pub fn compile_program(
    prog: &MirProgram,
    triple: Triple,
    object_name: &str,
) -> Result<Vec<u8>, CodegenError> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| CodegenError::Target(e.to_string()))?;

    let context = Context::create();
    let module = context.create_module(object_name);
    let llvm_triple = TargetTriple::create(&triple.to_string());
    module.set_triple(&llvm_triple);

    let target =
        Target::from_triple(&llvm_triple).map_err(|e| CodegenError::Target(e.to_string()))?;
    // PIC + opt-level=none, mirroring the Cranelift backend's choices in
    // `arsenal_codegen_fast`. Release-mode tuning is a Phase 8+ concern.
    let machine = target
        .create_target_machine(
            &llvm_triple,
            "generic",
            "",
            OptimizationLevel::None,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| CodegenError::Target("create_target_machine returned None".into()))?;
    module.set_data_layout(&machine.get_target_data().get_data_layout());

    // Pass 1: declare every function so calls can resolve forward.
    // External linkage is correct for both directions in LLVM — the
    // import/export distinction is implicit from whether the function
    // has a body.
    let mut fn_values: Vec<FunctionValue<'_>> = Vec::with_capacity(prog.functions.len());
    for f in &prog.functions {
        let fn_ty = make_fn_type(&context, f)?;
        let function = module.add_function(&f.name, fn_ty, Some(Linkage::External));
        fn_values.push(function);
    }

    // Pass 2: define non-extern bodies.
    for (i, f) in prog.functions.iter().enumerate() {
        if f.is_extern {
            continue;
        }
        define_fn(&context, &module, f, fn_values[i], &fn_values, prog)?;
    }

    // Verify the module before emitting. Catches malformed IR with a
    // useful error rather than letting `write_to_memory_buffer` produce
    // mystery output. Bootstrap-stage verification cost is negligible.
    module
        .verify()
        .map_err(|e| CodegenError::Builder(e.to_string()))?;

    let buffer = machine
        .write_to_memory_buffer(&module, FileType::Object)
        .map_err(|e| CodegenError::Emit(e.to_string()))?;
    Ok(buffer.as_slice().to_vec())
}

// ─── per-function definition ──────────────────────────────────────────

/// State threaded through statement / rvalue / operand lowering.
struct LoweringCx<'ctx, 'a> {
    context: &'ctx Context,
    /// Needed by `lower_cast` for intrinsic lookup
    /// (`llvm.fptosi.sat` / `llvm.fptoui.sat`).
    module: &'a Module<'ctx>,
    f: &'a MirFn,
    /// Stack-slot pointer per non-`u0` local. `u0` locals have no
    /// storage — they're never read or written.
    allocas: FxHashMap<Local, PointerValue<'ctx>>,
    /// Pre-created LLVM basic blocks, parallel to `f.blocks`.
    bbs: Vec<BasicBlock<'ctx>>,
    /// Function values declared in pass 1, indexed by [`FnIdx`].
    fn_values: &'a [FunctionValue<'ctx>],
    prog: &'a MirProgram,
}

fn define_fn<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    f: &MirFn,
    function: FunctionValue<'ctx>,
    fn_values: &[FunctionValue<'ctx>],
    prog: &MirProgram,
) -> Result<(), CodegenError> {
    let bbs: Vec<BasicBlock<'ctx>> = (0..f.blocks.len())
        .map(|i| context.append_basic_block(function, &format!("bb{i}")))
        .collect();

    let builder = context.create_builder();
    builder.position_at_end(bbs[0]);

    // Alloca every non-`u0` local. LLVM convention: allocas in the entry
    // block so mem2reg (when we add an opt pass later) can promote them
    // to SSA values.
    let mut allocas: FxHashMap<Local, PointerValue<'ctx>> = FxHashMap::default();
    for (i, decl) in f.locals.iter().enumerate() {
        if decl.ty == Ty::U0 {
            continue;
        }
        let lty = llvm_basic_type(context, decl.ty).ok_or_else(|| {
            CodegenError::Unsupported(format!(
                "fn `{}` local {} has type {:?}; B.3 supports integers, bool, and floats",
                f.name, i, decl.ty
            ))
        })?;
        let ptr = builder
            .build_alloca(lty, &format!("local{i}"))
            .map_err(|e| CodegenError::Builder(e.to_string()))?;
        allocas.insert(Local(i as u32), ptr);
    }

    // Store each fn parameter into its corresponding local's alloca, so
    // body reads load from the slot uniformly.
    for (i, &param_local) in f.params.iter().enumerate() {
        let ty = f.locals[param_local.0 as usize].ty;
        if ty == Ty::U0 {
            continue;
        }
        let val = function
            .get_nth_param(i as u32)
            .ok_or_else(|| CodegenError::Builder(format!("fn `{}` missing param {i}", f.name)))?;
        let ptr = allocas[&param_local];
        builder
            .build_store(ptr, val)
            .map_err(|e| CodegenError::Builder(e.to_string()))?;
    }

    let cx = LoweringCx {
        context,
        module,
        f,
        allocas,
        bbs,
        fn_values,
        prog,
    };

    // Lower each MIR block. The first iteration continues from the
    // entry block (where the allocas + param stores already live);
    // subsequent iterations switch to their own pre-created bb.
    for (i, mir_block) in f.blocks.iter().enumerate() {
        builder.position_at_end(cx.bbs[i]);
        for stmt in &mir_block.statements {
            lower_stmt(&builder, &cx, stmt)?;
        }
        lower_terminator(&builder, &cx, &mir_block.terminator, i)?;
    }
    Ok(())
}

// ─── statements ───────────────────────────────────────────────────────

fn lower_stmt<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    cx: &LoweringCx<'ctx, '_>,
    stmt: &MirStmt,
) -> Result<(), CodegenError> {
    match stmt {
        MirStmt::Assign { dst, value } => {
            let dst_ty = cx.f.locals[dst.0 as usize].ty;
            if dst_ty == Ty::U0 {
                // The MIR builder synthesises some `let _: u0 = expr;`
                // shapes whose Assign has no observable effect; skip.
                return Ok(());
            }
            let val = lower_rvalue(builder, cx, value, dst_ty)?;
            let ptr = cx.allocas[dst];
            builder
                .build_store(ptr, val)
                .map_err(|e| CodegenError::Builder(e.to_string()))?;
            Ok(())
        }
        MirStmt::AssignField { .. } => Err(CodegenError::Unsupported(
            "AssignField (class field write) — deferred to B.4".into(),
        )),
    }
}

// ─── rvalues ──────────────────────────────────────────────────────────

fn lower_rvalue<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    cx: &LoweringCx<'ctx, '_>,
    rv: &Rvalue,
    dst_ty: Ty,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    match rv {
        Rvalue::Use(op) => read_operand(builder, cx, op, dst_ty),
        Rvalue::BinOp { op, lhs, rhs, ty } => {
            let l = read_operand(builder, cx, lhs, *ty)?;
            let r = read_operand(builder, cx, rhs, *ty)?;
            lower_binop(builder, *op, *ty, l, r)
        }
        Rvalue::UnOp { op, operand, ty } => {
            let v = read_operand(builder, cx, operand, *ty)?;
            lower_unop(builder, *op, *ty, v)
        }
        Rvalue::Field { .. } => Err(CodegenError::Unsupported(
            "Rvalue::Field (class field read) — deferred to B.4".into(),
        )),
        Rvalue::Cast {
            kind,
            operand,
            src_ty,
            dst_ty,
        } => {
            let v = read_operand(builder, cx, operand, *src_ty)?;
            lower_cast(builder, cx, *kind, v, *src_ty, *dst_ty)
        }
    }
}

/// Lower an `as` cast. Each `CastKind` maps to one LLVM op (or no op
/// for the `*Bitcast` arms — LLVM integer types don't carry signedness,
/// and same-width float bitcast is identity).
///
/// Float→int uses LLVM's `llvm.fpto{si,ui}.sat` intrinsics so out-of-
/// range values saturate to dst::MIN/MAX and NaN→0 — matching Rust ≥
/// 1.45 / Cranelift `fcvt_to_*_sat`. The plain `fptosi` / `fptoui`
/// instructions have UB for the same inputs and are deliberately not
/// used. The intrinsics' overload signatures are
/// `<dst_int> @llvm.fptosi.sat.<dst_int>.<src_float>(<src_float>)`,
/// so we feed both type-overload tokens to `Intrinsic::get_declaration`.
fn lower_cast<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    cx: &LoweringCx<'ctx, '_>,
    kind: CastKind,
    v: BasicValueEnum<'ctx>,
    _src_ty: Ty,
    dst_ty: Ty,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let context = cx.context;
    Ok(match kind {
        CastKind::IntWiden { signed: true } => {
            let dst = match dst_ty {
                Ty::Int(it) => llvm_int_type(context, it),
                _ => return Err(unexpected_cast_dst("IntWiden", dst_ty)),
            };
            builder
                .build_int_s_extend(v.into_int_value(), dst, "sext")
                .map_err(be)?
                .into()
        }
        CastKind::IntWiden { signed: false } => {
            let dst = match dst_ty {
                Ty::Int(it) => llvm_int_type(context, it),
                _ => return Err(unexpected_cast_dst("IntWiden", dst_ty)),
            };
            builder
                .build_int_z_extend(v.into_int_value(), dst, "zext")
                .map_err(be)?
                .into()
        }
        CastKind::IntTrunc => {
            let dst = match dst_ty {
                Ty::Int(it) => llvm_int_type(context, it),
                _ => return Err(unexpected_cast_dst("IntTrunc", dst_ty)),
            };
            builder
                .build_int_truncate(v.into_int_value(), dst, "trunc")
                .map_err(be)?
                .into()
        }
        // Same-width signedness reinterpretation: LLVM integers are
        // unsigned-by-bit-pattern, identical to Cranelift, so the
        // operand value is already correct.
        CastKind::IntBitcast => v,
        CastKind::IntToFloat { signed: true } => {
            let dst = match dst_ty {
                Ty::Float(ft) => llvm_float_type(context, ft),
                _ => return Err(unexpected_cast_dst("IntToFloat", dst_ty)),
            };
            builder
                .build_signed_int_to_float(v.into_int_value(), dst, "sitofp")
                .map_err(be)?
                .into()
        }
        CastKind::IntToFloat { signed: false } => {
            let dst = match dst_ty {
                Ty::Float(ft) => llvm_float_type(context, ft),
                _ => return Err(unexpected_cast_dst("IntToFloat", dst_ty)),
            };
            builder
                .build_unsigned_int_to_float(v.into_int_value(), dst, "uitofp")
                .map_err(be)?
                .into()
        }
        CastKind::FloatToInt { signed } => {
            let dst = match dst_ty {
                Ty::Int(it) => llvm_int_type(context, it),
                _ => return Err(unexpected_cast_dst("FloatToInt", dst_ty)),
            };
            let src_fv = v.into_float_value();
            let intrinsic_name = if signed {
                "llvm.fptosi.sat"
            } else {
                "llvm.fptoui.sat"
            };
            let intrinsic = Intrinsic::find(intrinsic_name).ok_or_else(|| {
                CodegenError::Builder(format!("intrinsic `{intrinsic_name}` not found"))
            })?;
            // Overload tokens: <dst_int>, <src_float>. inkwell looks up
            // (or creates) the per-overload declaration in the module.
            let func = intrinsic
                .get_declaration(cx.module, &[dst.into(), src_fv.get_type().into()])
                .ok_or_else(|| {
                    CodegenError::Builder(format!(
                        "intrinsic `{intrinsic_name}` declaration with overload \
                         (dst={dst:?}, src={:?}) not found",
                        src_fv.get_type()
                    ))
                })?;
            let call = builder
                .build_call(func, &[src_fv.into()], "fcvtsat")
                .map_err(be)?;
            call.try_as_basic_value().left().ok_or_else(|| {
                CodegenError::Builder(format!(
                    "intrinsic `{intrinsic_name}` returned void unexpectedly"
                ))
            })?
        }
        CastKind::FloatExt => {
            let dst = match dst_ty {
                Ty::Float(ft) => llvm_float_type(context, ft),
                _ => return Err(unexpected_cast_dst("FloatExt", dst_ty)),
            };
            builder
                .build_float_ext(v.into_float_value(), dst, "fpext")
                .map_err(be)?
                .into()
        }
        CastKind::FloatTrunc => {
            let dst = match dst_ty {
                Ty::Float(ft) => llvm_float_type(context, ft),
                _ => return Err(unexpected_cast_dst("FloatTrunc", dst_ty)),
            };
            builder
                .build_float_trunc(v.into_float_value(), dst, "fptrunc")
                .map_err(be)?
                .into()
        }
        // Same float width: nothing to do.
        CastKind::FloatBitcast => v,
    })
}

fn unexpected_cast_dst(kind: &str, dst_ty: Ty) -> CodegenError {
    CodegenError::Builder(format!(
        "CastKind::{kind} produced for non-matching dst type {dst_ty:?}"
    ))
}

fn lower_binop<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    op: BinOp,
    ty: Ty,
    l: BasicValueEnum<'ctx>,
    r: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if ty.is_float() {
        return lower_float_binop(builder, op, l.into_float_value(), r.into_float_value());
    }
    let l_int = l.into_int_value();
    let r_int = r.into_int_value();
    let signed = ty.is_signed_int();
    let v: IntValue<'ctx> = match op {
        BinOp::Add => builder.build_int_add(l_int, r_int, "add").map_err(be)?,
        BinOp::Sub => builder.build_int_sub(l_int, r_int, "sub").map_err(be)?,
        BinOp::Mul => builder.build_int_mul(l_int, r_int, "mul").map_err(be)?,
        BinOp::Div => {
            if signed {
                builder
                    .build_int_signed_div(l_int, r_int, "sdiv")
                    .map_err(be)?
            } else {
                builder
                    .build_int_unsigned_div(l_int, r_int, "udiv")
                    .map_err(be)?
            }
        }
        BinOp::Mod => {
            if signed {
                builder
                    .build_int_signed_rem(l_int, r_int, "srem")
                    .map_err(be)?
            } else {
                builder
                    .build_int_unsigned_rem(l_int, r_int, "urem")
                    .map_err(be)?
            }
        }
        BinOp::Pow => {
            return Err(CodegenError::Unsupported(
                "integer `**` (Pow) — typeck doesn't currently emit this; \
                 add a corpus program first if needed"
                    .into(),
            ));
        }
        BinOp::BitAnd => builder.build_and(l_int, r_int, "and").map_err(be)?,
        BinOp::BitOr => builder.build_or(l_int, r_int, "or").map_err(be)?,
        BinOp::BitXor => builder.build_xor(l_int, r_int, "xor").map_err(be)?,
        BinOp::Shl => builder.build_left_shift(l_int, r_int, "shl").map_err(be)?,
        BinOp::Shr => builder
            .build_right_shift(l_int, r_int, signed, "shr")
            .map_err(be)?,
        // `LogAnd` / `LogOr` shouldn't reach codegen — MIR lowers `&&`
        // and `||` to short-circuit control flow (HANDOFF decision #15).
        // Keep the eager path as a safety net so legal MIR doesn't crash;
        // observable-effect skipping is already guaranteed by lowering.
        BinOp::LogAnd => builder.build_and(l_int, r_int, "logand").map_err(be)?,
        BinOp::LogOr => builder.build_or(l_int, r_int, "logor").map_err(be)?,
        BinOp::Eq => builder
            .build_int_compare(IntPredicate::EQ, l_int, r_int, "eq")
            .map_err(be)?,
        BinOp::Ne => builder
            .build_int_compare(IntPredicate::NE, l_int, r_int, "ne")
            .map_err(be)?,
        BinOp::Lt => builder
            .build_int_compare(
                if signed {
                    IntPredicate::SLT
                } else {
                    IntPredicate::ULT
                },
                l_int,
                r_int,
                "lt",
            )
            .map_err(be)?,
        BinOp::Le => builder
            .build_int_compare(
                if signed {
                    IntPredicate::SLE
                } else {
                    IntPredicate::ULE
                },
                l_int,
                r_int,
                "le",
            )
            .map_err(be)?,
        BinOp::Gt => builder
            .build_int_compare(
                if signed {
                    IntPredicate::SGT
                } else {
                    IntPredicate::UGT
                },
                l_int,
                r_int,
                "gt",
            )
            .map_err(be)?,
        BinOp::Ge => builder
            .build_int_compare(
                if signed {
                    IntPredicate::SGE
                } else {
                    IntPredicate::UGE
                },
                l_int,
                r_int,
                "ge",
            )
            .map_err(be)?,
    };
    Ok(v.into())
}

/// Float arms split out of [`lower_binop`] so the int path stays
/// readable. Comparisons use ordered LLVM predicates (`OEQ`, `OLT`,
/// etc.) which return false against NaN — same semantics as the
/// Cranelift backend's `FloatCC::Equal` family. `Mod` and `Pow` on
/// floats are not produced by typeck today; left as Unsupported until
/// they show up in a corpus program.
fn lower_float_binop<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    op: BinOp,
    l: FloatValue<'ctx>,
    r: FloatValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let v: BasicValueEnum<'ctx> = match op {
        BinOp::Add => builder.build_float_add(l, r, "fadd").map_err(be)?.into(),
        BinOp::Sub => builder.build_float_sub(l, r, "fsub").map_err(be)?.into(),
        BinOp::Mul => builder.build_float_mul(l, r, "fmul").map_err(be)?.into(),
        BinOp::Div => builder.build_float_div(l, r, "fdiv").map_err(be)?.into(),
        BinOp::Mod | BinOp::Pow => {
            return Err(CodegenError::Unsupported(format!(
                "BinOp::{op:?} on float operands — typeck doesn't currently emit this"
            )));
        }
        BinOp::BitAnd
        | BinOp::BitOr
        | BinOp::BitXor
        | BinOp::Shl
        | BinOp::Shr
        | BinOp::LogAnd
        | BinOp::LogOr => {
            return Err(CodegenError::Unsupported(format!(
                "BinOp::{op:?} on float operands — typeck rejects bitwise / logical ops on floats"
            )));
        }
        BinOp::Eq => builder
            .build_float_compare(FloatPredicate::OEQ, l, r, "feq")
            .map_err(be)?
            .into(),
        BinOp::Ne => builder
            .build_float_compare(FloatPredicate::ONE, l, r, "fne")
            .map_err(be)?
            .into(),
        BinOp::Lt => builder
            .build_float_compare(FloatPredicate::OLT, l, r, "flt")
            .map_err(be)?
            .into(),
        BinOp::Le => builder
            .build_float_compare(FloatPredicate::OLE, l, r, "fle")
            .map_err(be)?
            .into(),
        BinOp::Gt => builder
            .build_float_compare(FloatPredicate::OGT, l, r, "fgt")
            .map_err(be)?
            .into(),
        BinOp::Ge => builder
            .build_float_compare(FloatPredicate::OGE, l, r, "fge")
            .map_err(be)?
            .into(),
    };
    Ok(v)
}

fn lower_unop<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    op: UnOp,
    ty: Ty,
    v: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if ty.is_float() {
        return match op {
            UnOp::Neg => Ok(builder
                .build_float_neg(v.into_float_value(), "fneg")
                .map_err(be)?
                .into()),
            UnOp::Not | UnOp::BitNot => Err(CodegenError::Unsupported(format!(
                "UnOp::{op:?} on float operand — typeck rejects this"
            ))),
        };
    }
    let v_int = v.into_int_value();
    let result: IntValue<'ctx> = match op {
        UnOp::Neg => builder.build_int_neg(v_int, "neg").map_err(be)?,
        // Logical not on bool: XOR with 1. `build_not` is bitwise — for
        // an i1 it agrees with logical-not, but for any wider integer
        // (which Rust spells as `!u8` etc.) typeck routes through
        // `BitNot` instead. So `Not` is bool-only by construction.
        UnOp::Not => {
            let one = cx_for_int(builder, v_int).const_int(1, false);
            builder.build_xor(v_int, one, "not").map_err(be)?
        }
        UnOp::BitNot => builder.build_not(v_int, "bnot").map_err(be)?,
    };
    Ok(result.into())
}

/// inkwell helper — fetch the int type of a value (so unop can build a
/// matching constant without re-deriving the type from `Ty`).
fn cx_for_int<'ctx>(
    _builder: &inkwell::builder::Builder<'ctx>,
    v: IntValue<'ctx>,
) -> IntType<'ctx> {
    v.get_type()
}

fn be(e: inkwell::builder::BuilderError) -> CodegenError {
    CodegenError::Builder(e.to_string())
}

// ─── operands ─────────────────────────────────────────────────────────

fn read_operand<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    cx: &LoweringCx<'ctx, '_>,
    op: &Operand,
    ty: Ty,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    match op {
        Operand::Const(c) => emit_const(builder, cx.context, c, ty),
        Operand::Local(l) => {
            let ptr = *cx.allocas.get(l).ok_or_else(|| {
                CodegenError::Builder(format!(
                    "fn `{}` reads local {l:?} that has no alloca (likely u0)",
                    cx.f.name
                ))
            })?;
            let lty = llvm_basic_type(cx.context, ty).ok_or_else(|| {
                CodegenError::Unsupported(format!(
                    "fn `{}` reads local {l:?} with type {:?}; B.3 supports integers, bool, and floats",
                    cx.f.name, ty
                ))
            })?;
            builder.build_load(lty, ptr, "load").map_err(be)
        }
    }
}

fn emit_const<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    context: &'ctx Context,
    c: &Const,
    ty: Ty,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    match c {
        Const::Int { value, ty: int_ty } => {
            let lty = llvm_int_type(context, *int_ty);
            // `as u64` reinterprets the i128's low 64 bits. For all
            // I8..I64 widths (including ISize/USize on 64-bit hosts)
            // the truncation is exact: typeck already bounded the
            // literal to fit `int_ty`. `sign_extend = false` because
            // inkwell takes the bits literally.
            Ok(lty.const_int(*value as u64, false).into())
        }
        Const::Bool(b) => Ok(context
            .bool_type()
            .const_int(if *b { 1 } else { 0 }, false)
            .into()),
        Const::Unit => {
            // `u0` has no LLVM value. Callers that hit this in B.2 are
            // either bugs in MIR lowering or the rare `Return(Unit)`
            // for `u0`-returning fns; that path takes a separate code
            // path in `lower_terminator`. Surface as Unsupported to
            // avoid silent miscodegen.
            Err(CodegenError::Unsupported(format!(
                "`Const::Unit` used as a value in non-void context (expected ty {ty:?})"
            )))
        }
        Const::Float { bits, ty: float_ty } => {
            // Build the constant by bitcasting an integer of matching
            // width to the float type at runtime. LLVM constant-folds
            // this immediately so it ends up as a literal in `.text`
            // — but going through bitcast (rather than
            // `FloatType::const_float(f64)`) preserves NaN payloads
            // exactly, which an `f64` round-trip on the F32 path would
            // lose.
            let f_ty = llvm_float_type(context, *float_ty);
            let v = match float_ty {
                FloatTy::F32 => {
                    let int_const = context.i32_type().const_int(*bits & 0xFFFF_FFFF, false);
                    builder
                        .build_bit_cast(int_const, f_ty, "fbits")
                        .map_err(be)?
                }
                FloatTy::F64 => {
                    let int_const = context.i64_type().const_int(*bits, false);
                    builder
                        .build_bit_cast(int_const, f_ty, "fbits")
                        .map_err(be)?
                }
            };
            Ok(v)
        }
        Const::DataAddr(_) => Err(CodegenError::Unsupported(
            "`Const::DataAddr` (string literal) — deferred to B.5".into(),
        )),
        Const::Error => Err(CodegenError::Unsupported(
            "`Const::Error` reached codegen; typeck should have errored".into(),
        )),
    }
}

// ─── terminators ──────────────────────────────────────────────────────

fn lower_terminator<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    cx: &LoweringCx<'ctx, '_>,
    term: &Terminator,
    block_idx: usize,
) -> Result<(), CodegenError> {
    match term {
        Terminator::Goto(target) => {
            let dest = cx.bbs[target.0 as usize];
            builder.build_unconditional_branch(dest).map_err(be)?;
        }
        Terminator::Branch {
            cond,
            then_bb,
            else_bb,
        } => {
            let v = read_operand(builder, cx, cond, Ty::Bool)?.into_int_value();
            // The MIR holds bool conditions at i1 already (typeck
            // normalises `Branch`'s cond to `Ty::Bool`). LLVM's
            // `br i1` requires exactly i1, so no zext/trunc needed.
            let then_bb = cx.bbs[then_bb.0 as usize];
            let else_bb = cx.bbs[else_bb.0 as usize];
            builder
                .build_conditional_branch(v, then_bb, else_bb)
                .map_err(be)?;
        }
        Terminator::Return(op) => {
            if matches!(cx.f.return_ty, Ty::U0 | Ty::Error) {
                builder.build_return(None).map_err(be)?;
            } else {
                let v = read_operand(builder, cx, op, cx.f.return_ty)?;
                builder.build_return(Some(&v)).map_err(be)?;
            }
        }
        Terminator::Call {
            callee,
            args,
            dst,
            target_bb,
        } => {
            lower_call(builder, cx, *callee, args, *dst, *target_bb)?;
        }
        Terminator::Unreachable => {
            builder.build_unreachable().map_err(be)?;
        }
    }
    let _ = block_idx;
    Ok(())
}

fn lower_call<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    cx: &LoweringCx<'ctx, '_>,
    callee: FnIdx,
    args: &[Operand],
    dst: Local,
    target_bb: BlockId,
) -> Result<(), CodegenError> {
    let callee_fn = &cx.prog.functions[callee.0 as usize];
    let callee_value = cx.fn_values[callee.0 as usize];

    let mut arg_vals: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
    for (i, op) in args.iter().enumerate() {
        let param_local = callee_fn.params[i];
        let param_ty = callee_fn.locals[param_local.0 as usize].ty;
        let v = read_operand(builder, cx, op, param_ty)?;
        arg_vals.push(v.into());
    }

    let call_site = builder
        .build_call(callee_value, &arg_vals, "call")
        .map_err(be)?;

    let dst_ty = cx.f.locals[dst.0 as usize].ty;
    if !matches!(dst_ty, Ty::U0 | Ty::Error) {
        // Scalar return: store into the dst local's alloca. (Aggregate
        // returns land in B.4 via a hidden out-pointer.)
        let ret = call_site
            .try_as_basic_value()
            .left()
            .ok_or_else(|| CodegenError::Builder("call returned void to non-u0 dst".into()))?;
        let ptr = cx.allocas[&dst];
        builder.build_store(ptr, ret).map_err(be)?;
    }
    let target = cx.bbs[target_bb.0 as usize];
    builder.build_unconditional_branch(target).map_err(be)?;
    Ok(())
}

// ─── type mapping ─────────────────────────────────────────────────────

fn make_fn_type<'ctx>(
    context: &'ctx Context,
    f: &MirFn,
) -> Result<FunctionType<'ctx>, CodegenError> {
    let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::with_capacity(f.params.len());
    for &param_local in &f.params {
        let pty = f.locals[param_local.0 as usize].ty;
        let lty = llvm_basic_type(context, pty).ok_or_else(|| {
            CodegenError::Unsupported(format!(
                "fn `{}` param {:?} type {:?}; B.3 supports integers, bool, and floats",
                f.name, param_local, pty
            ))
        })?;
        params.push(lty.into());
    }
    Ok(match f.return_ty {
        Ty::U0 => context.void_type().fn_type(&params, false),
        Ty::Int(int_ty) => llvm_int_type(context, int_ty).fn_type(&params, false),
        Ty::Bool => context.bool_type().fn_type(&params, false),
        Ty::Float(float_ty) => llvm_float_type(context, float_ty).fn_type(&params, false),
        ref other => {
            return Err(CodegenError::Unsupported(format!(
                "fn `{}` return type {:?}; B.3 supports `u0`, integers, bool, and floats",
                f.name, other
            )));
        }
    })
}

/// Map a [`Ty`] to its LLVM `BasicTypeEnum`, or `None` for `u0` and
/// any type B.3 doesn't yet handle (caller turns `None` into an
/// `Unsupported` error).
fn llvm_basic_type<'ctx>(context: &'ctx Context, ty: Ty) -> Option<BasicTypeEnum<'ctx>> {
    match ty {
        Ty::U0 => None,
        Ty::Bool => Some(context.bool_type().into()),
        Ty::Int(int_ty) => Some(llvm_int_type(context, int_ty).into()),
        Ty::Float(float_ty) => Some(llvm_float_type(context, float_ty).into()),
        // Aggregates / pointers in B.4+.
        Ty::Class(_) | Ty::Slice(_) | Ty::Ptr(_) | Ty::Rune | Ty::Error => None,
        // `Ty` is non-exhaustive (Phase 2 will add `?T`, `!T`, etc.);
        // anything new lands here as Unsupported until handled.
        _ => None,
    }
}

fn llvm_int_type<'ctx>(context: &'ctx Context, ty: IntTy) -> IntType<'ctx> {
    match ty {
        IntTy::I8 | IntTy::U8 => context.i8_type(),
        IntTy::I16 | IntTy::U16 => context.i16_type(),
        IntTy::I32 | IntTy::U32 => context.i32_type(),
        IntTy::I64 | IntTy::U64 => context.i64_type(),
        // ISize/USize on every Phase-1 target is 64-bit; revisit when a
        // 32-bit target ships. Matches the typeck simplification noted
        // in HANDOFF decision #16.
        IntTy::ISize | IntTy::USize => context.i64_type(),
    }
}

fn llvm_float_type<'ctx>(context: &'ctx Context, ty: FloatTy) -> FloatType<'ctx> {
    match ty {
        FloatTy::F32 => context.f32_type(),
        FloatTy::F64 => context.f64_type(),
    }
}
