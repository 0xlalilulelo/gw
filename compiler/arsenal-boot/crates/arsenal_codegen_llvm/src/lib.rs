//! GW LLVM backend (Phase 13).
//!
//! Mirrors the public contract of [`arsenal_codegen_fast::compile_program`]:
//! `MirProgram → object bytes`. The driver picks between the two backends
//! at `arsenal build --backend=fast|llvm`.
//!
//! Supported MIR subset (B.1 → B.5):
//! - integer + bool + float + class + slice params and returns; `u0`
//!   returns
//! - alloca-backed locals for every type (primitives use a typed alloca;
//!   classes / slices use an `[N x i8]` alloca sized to the layout)
//! - `Rvalue::Use` / `BinOp` / `UnOp` over integers, bools, and floats
//!   (float comparison via `OEQ`/`ONE`/`OLT`/etc., matching the
//!   Cranelift backend's "ordered, NaN→false" semantics)
//! - `Rvalue::Cast` across the full numeric matrix: int↔int (sext /
//!   zext / trunc / no-op), int↔float (sitofp / uitofp / saturating
//!   fptosi / fptoui via `llvm.fpto{si,ui}.sat` intrinsics), float↔float
//!   (fpext / fptrunc / no-op). Float→int matches Rust ≥ 1.45 / Cranelift
//!   `fcvt_to_*_sat`: saturating with NaN→0, no extra branch needed.
//! - `Rvalue::Field` / `MirStmt::AssignField` over class-typed locals via
//!   `getelementptr` at byte offsets computed from the typeck `ClassLayout`
//! - `Rvalue::Use(Operand::Local(src))` into an aggregate-typed dst
//!   lowers to an `llvm.memcpy` between the slots
//! - aggregate ABI: hidden out-pointer for class- / slice-returning fns,
//!   by-pointer for class- / slice-typed user params (HANDOFF decision
//!   #21). `sret`/`byval` attributes are intentionally omitted: corpus
//!   aggregates flow only between GW fns, not through C ABI, and the
//!   plain-`ptr` form agrees with Cranelift's manual `stack_addr`
//!   convention end-to-end.
//! - string literals: one private `__gw_str_<i>` global per entry in
//!   `MirProgram::string_literals`, populated with the decoded bytes.
//!   `Const::DataAddr(id)` materialises as the global's address (an
//!   opaque `ptr`). The slice value lowers via two `AssignField`s into
//!   an `[]u8` aggregate slot; the implicit Print desugar then reads
//!   `slice.data` (typed `*u8`) and `slice.len` (typed `usize`) and
//!   passes them to an auto-injected `extern fn write`.
//! - `*T` raw pointers in extern fn signatures (per HANDOFF #13) and as
//!   the type of `slice.data` lower as opaque `ptr`. Non-extern uses
//!   stay rejected by typeck.
//! - `Operand::Const(Int|Bool|Float|Unit|DataAddr)`, `Operand::Local`
//! - `Terminator::{Goto, Branch, Return, Call, Unreachable}`
//!
//! Anything outside the supported set returns
//! [`CodegenError::Unsupported`] with a descriptive message.

use arsenal_mir::{
    BinOp, BlockId, CastKind, Const, FnIdx, Local, MirFn, MirProgram, MirStmt, Operand, Rvalue,
    Terminator, UnOp,
};
use arsenal_typeck::{ClassLayout, FloatTy, IntTy, Ty};
use inkwell::basic_block::BasicBlock;
use inkwell::context::Context;
use inkwell::intrinsics::Intrinsic;
use inkwell::module::{Linkage, Module};
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetTriple,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, FloatType, FunctionType, IntType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FloatValue, FunctionValue, GlobalValue, IntValue,
    PointerValue,
};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel};
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

    // Pass 1b: declare and initialise one read-only global per string
    // literal in the MIR. `Const::DataAddr(id)` lowers to the matching
    // global's address. Mirrors `arsenal_codegen_fast`'s rodata pass —
    // same naming (`__gw_str_<i>`), same private linkage, so binaries
    // produced by either backend look the same to a disassembler.
    let mut string_globals: Vec<GlobalValue<'_>> = Vec::with_capacity(prog.string_literals.len());
    for (i, bytes) in prog.string_literals.iter().enumerate() {
        // Cranelift pads zero-length payloads to one byte so the symbol
        // still resolves; do the same so the two backends emit
        // structurally-identical objects. The GW-level `len` comes from
        // the slice's `len` field (set by the Const::Int AssignField),
        // so a 1-byte real payload still reads as len=0.
        let payload: Vec<u8> = if bytes.is_empty() {
            vec![0]
        } else {
            bytes.clone()
        };
        let const_str = context.const_string(&payload, false);
        let global = module.add_global(const_str.get_type(), None, &format!("__gw_str_{i}"));
        global.set_initializer(&const_str);
        global.set_constant(true);
        global.set_linkage(Linkage::Private);
        global.set_unnamed_addr(true);
        string_globals.push(global);
    }

    // Pass 2: define non-extern bodies.
    for (i, f) in prog.functions.iter().enumerate() {
        if f.is_extern {
            continue;
        }
        define_fn(
            &context,
            &module,
            f,
            fn_values[i],
            &fn_values,
            &string_globals,
            prog,
        )?;
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
    /// (`llvm.fptosi.sat` / `llvm.fptoui.sat`) and by aggregate-arg /
    /// memcpy paths (intrinsic-based memcpy).
    module: &'a Module<'ctx>,
    f: &'a MirFn,
    /// Stack-slot pointer per non-`u0` local. Aggregate locals share
    /// the same map: their alloca is a sized `[N x i8]` whose address
    /// is the slot's base pointer.
    allocas: FxHashMap<Local, PointerValue<'ctx>>,
    /// Pre-created LLVM basic blocks, parallel to `f.blocks`.
    bbs: Vec<BasicBlock<'ctx>>,
    /// Function values declared in pass 1, indexed by [`FnIdx`].
    fn_values: &'a [FunctionValue<'ctx>],
    /// Global values for each string literal, indexed by `StringLitId`.
    /// `Const::DataAddr(id)` reads its address.
    string_globals: &'a [GlobalValue<'ctx>],
    prog: &'a MirProgram,
    /// Hidden out-pointer for aggregate-returning fns, captured at fn
    /// entry from the prepended LLVM param. Used by `Terminator::Return`
    /// to memcpy the result slot through and emit a void return.
    ret_out_ptr: Option<PointerValue<'ctx>>,
    /// Native pointer width in bytes (8 for every Phase-1 target).
    /// Slice layout uses this; aggregate alignments are derived here.
    ptr_bytes: u32,
}

// ─── aggregate layout helpers ─────────────────────────────────────────

/// Whether `ty` is passed and returned by hidden pointer in Phase 1.
/// Mirrors `arsenal_codegen_fast::is_aggregate_ty` — same rule on both
/// backends so the MIR-level ABI invariants stay aligned.
fn is_aggregate_ty(ty: Ty) -> bool {
    matches!(ty, Ty::Class(_) | Ty::Slice(_))
}

/// Computed layout for a class or slice: total byte size, max-field
/// alignment, and the byte offset of each field.
struct ResolvedClassLayout {
    size: u32,
    align: u32,
    offsets: Vec<u32>,
}

fn resolve_class_layout(layout: &ClassLayout, ptr_bytes: u32) -> ResolvedClassLayout {
    let mut offsets = Vec::with_capacity(layout.fields.len());
    let mut offset: u32 = 0;
    let mut max_align: u32 = 1;
    for f in &layout.fields {
        let (sz, al) = primitive_size_align(f.ty, ptr_bytes);
        offset = align_up(offset, al);
        offsets.push(offset);
        offset = offset.saturating_add(sz);
        if al > max_align {
            max_align = al;
        }
    }
    let size = align_up(offset, max_align);
    ResolvedClassLayout {
        size,
        align: max_align,
        offsets,
    }
}

fn primitive_size_align(ty: Ty, ptr_bytes: u32) -> (u32, u32) {
    match ty {
        Ty::U0 => (0, 1),
        Ty::Bool => (1, 1),
        Ty::Int(IntTy::I8) | Ty::Int(IntTy::U8) => (1, 1),
        Ty::Int(IntTy::I16) | Ty::Int(IntTy::U16) => (2, 2),
        Ty::Int(IntTy::I32) | Ty::Int(IntTy::U32) => (4, 4),
        Ty::Int(IntTy::I64) | Ty::Int(IntTy::U64) => (8, 8),
        Ty::Int(IntTy::ISize) | Ty::Int(IntTy::USize) => (ptr_bytes, ptr_bytes),
        Ty::Float(FloatTy::F32) => (4, 4),
        Ty::Float(FloatTy::F64) => (8, 8),
        Ty::Rune => (4, 4),
        Ty::Ptr(_) => (ptr_bytes, ptr_bytes),
        // Phase 1 doesn't have nested-class fields. Fall back to
        // pointer-sized for safety.
        _ => (ptr_bytes, ptr_bytes),
    }
}

const fn align_up(v: u32, align: u32) -> u32 {
    if align <= 1 {
        return v;
    }
    let mask = align - 1;
    (v + mask) & !mask
}

/// Compute the layout of an aggregate (class or slice) local. Slices
/// have a fixed two-field shape `(data: ptr@0, len: usize@ptr_bytes)`,
/// matching the Cranelift backend exactly so by-pointer ABI agrees on
/// both sides.
fn aggregate_layout(ty: Ty, prog: &MirProgram, ptr_bytes: u32) -> Option<ResolvedClassLayout> {
    match ty {
        Ty::Class(def_id) => prog
            .class_layouts
            .get(&def_id)
            .map(|cl| resolve_class_layout(cl, ptr_bytes)),
        Ty::Slice(_) => Some(ResolvedClassLayout {
            size: 2 * ptr_bytes,
            align: ptr_bytes,
            offsets: vec![0, ptr_bytes],
        }),
        _ => None,
    }
}

/// The GW-level type of an aggregate's `field_idx`th field, used by
/// codegen to pick the load/store width. Slice fields are reported as
/// `Ty::Int(IntTy::USize)` for both `data` and `len`: the data pointer
/// is pointer-sized, which yields the correct LLVM load/store width
/// even though the source-level `Ty` would be `*u8`. Mirrors the
/// Cranelift backend's `aggregate_field_ty`.
fn aggregate_field_ty(ty: Ty, field_idx: u32, prog: &MirProgram) -> Ty {
    match ty {
        Ty::Class(def_id) => prog
            .class_layouts
            .get(&def_id)
            .and_then(|cl| cl.fields.get(field_idx as usize))
            .map(|f| f.ty)
            .unwrap_or(Ty::Error),
        Ty::Slice(_) => Ty::Int(IntTy::USize),
        _ => Ty::Error,
    }
}

#[allow(clippy::too_many_arguments)]
fn define_fn<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    f: &MirFn,
    function: FunctionValue<'ctx>,
    fn_values: &[FunctionValue<'ctx>],
    string_globals: &[GlobalValue<'ctx>],
    prog: &MirProgram,
) -> Result<(), CodegenError> {
    // Pointer width — Phase-1 targets are all 64-bit (HANDOFF #16 /
    // matches the typeck simplification for ISize/USize). Hardcoded
    // here for the same reason the int_type path uses i64; revisit
    // when a 32-bit target ships.
    let ptr_bytes: u32 = 8;

    let returns_aggregate = is_aggregate_ty(f.return_ty);

    let bbs: Vec<BasicBlock<'ctx>> = (0..f.blocks.len())
        .map(|i| context.append_basic_block(function, &format!("bb{i}")))
        .collect();

    let builder = context.create_builder();
    builder.position_at_end(bbs[0]);

    // Alloca every non-`u0` local. LLVM convention: allocas in the entry
    // block so mem2reg (when we add an opt pass later) can promote them
    // to SSA values. Aggregates use a sized `[N x i8]` alloca with the
    // layout's natural alignment so f64-bearing classes stay aligned;
    // primitives use a typed alloca matching their LLVM type.
    let mut allocas: FxHashMap<Local, PointerValue<'ctx>> = FxHashMap::default();
    for (i, decl) in f.locals.iter().enumerate() {
        if decl.ty == Ty::U0 {
            continue;
        }
        let local = Local(i as u32);
        let ptr = if is_aggregate_ty(decl.ty) {
            let layout = aggregate_layout(decl.ty, prog, ptr_bytes).ok_or_else(|| {
                CodegenError::Builder(format!(
                    "fn `{}` aggregate local {i} has no layout (def_id missing from class_layouts?)",
                    f.name
                ))
            })?;
            let arr_ty = context.i8_type().array_type(layout.size.max(1));
            let p = builder
                .build_alloca(arr_ty, &format!("local{i}_agg"))
                .map_err(be)?;
            // Bump alignment to the layout's natural one so f64/i64
            // class fields aren't unaligned. inkwell exposes
            // `set_alignment` via the underlying instruction value.
            if let Some(inst) = p.as_instruction() {
                inst.set_alignment(layout.align.max(1)).map_err(|e| {
                    CodegenError::Builder(format!("set_alignment on aggregate alloca failed: {e}"))
                })?;
            }
            p
        } else {
            let lty = llvm_basic_type(context, decl.ty).ok_or_else(|| {
                CodegenError::Unsupported(format!(
                    "fn `{}` local {i} has type {:?}; B.4 supports integers, bool, floats, classes, and slices",
                    f.name, decl.ty
                ))
            })?;
            builder
                .build_alloca(lty, &format!("local{i}"))
                .map_err(be)?
        };
        allocas.insert(local, ptr);
    }

    // Capture the hidden out-pointer (LLVM param 0) for aggregate-
    // returning fns. The remaining LLVM params shift right by one.
    let ret_out_ptr = if returns_aggregate {
        let p = function
            .get_nth_param(0)
            .ok_or_else(|| CodegenError::Builder(format!("fn `{}` missing sret out-ptr", f.name)))?
            .into_pointer_value();
        Some(p)
    } else {
        None
    };

    // Store each fn parameter into its corresponding local's alloca:
    // primitives `store` the SSA value, aggregates `memcpy` from the
    // incoming pointer into the local's slot so body field accesses
    // hit a fresh copy (pass-by-value at the source level — HANDOFF #21).
    let llvm_param_offset = if returns_aggregate { 1 } else { 0 };
    for (i, &param_local) in f.params.iter().enumerate() {
        let ty = f.locals[param_local.0 as usize].ty;
        if ty == Ty::U0 {
            continue;
        }
        let llvm_idx = (i + llvm_param_offset) as u32;
        let val = function
            .get_nth_param(llvm_idx)
            .ok_or_else(|| CodegenError::Builder(format!("fn `{}` missing param {i}", f.name)))?;
        let dst_ptr = allocas[&param_local];
        if is_aggregate_ty(ty) {
            let layout = aggregate_layout(ty, prog, ptr_bytes).ok_or_else(|| {
                CodegenError::Builder(format!("fn `{}` aggregate param {i} has no layout", f.name))
            })?;
            let src_ptr = val.into_pointer_value();
            let size = context.i64_type().const_int(layout.size as u64, false);
            builder
                .build_memcpy(
                    dst_ptr,
                    layout.align.max(1),
                    src_ptr,
                    layout.align.max(1),
                    size,
                )
                .map_err(|e| CodegenError::Builder(format!("memcpy param: {e}")))?;
        } else {
            builder.build_store(dst_ptr, val).map_err(be)?;
        }
    }

    let cx = LoweringCx {
        context,
        module,
        f,
        allocas,
        bbs,
        fn_values,
        string_globals,
        prog,
        ret_out_ptr,
        ptr_bytes,
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
            if is_aggregate_ty(dst_ty) {
                // Aggregate-typed Assign: only `Use(Local(src))` is
                // legal in Phase 1 (let-init shadowing of a struct/
                // string literal temp). Lower as a slot-to-slot
                // memcpy. Other rvalue shapes shouldn't be produced
                // by lowering; surface as Unsupported defensively.
                let src_local = match value {
                    Rvalue::Use(Operand::Local(l)) => l,
                    _ => {
                        return Err(CodegenError::Unsupported(format!(
                            "aggregate-typed Assign with non-Local rvalue (dst {:?}, ty {:?})",
                            dst, dst_ty
                        )));
                    }
                };
                let layout = aggregate_layout(dst_ty, cx.prog, cx.ptr_bytes)
                    .ok_or_else(|| CodegenError::Builder("aggregate Assign: no layout".into()))?;
                let dst_ptr = cx.allocas[dst];
                let src_ptr = cx.allocas[src_local];
                let size = cx.context.i64_type().const_int(layout.size as u64, false);
                builder
                    .build_memcpy(
                        dst_ptr,
                        layout.align.max(1),
                        src_ptr,
                        layout.align.max(1),
                        size,
                    )
                    .map_err(|e| CodegenError::Builder(format!("memcpy assign: {e}")))?;
                return Ok(());
            }
            let val = lower_rvalue(builder, cx, value, dst_ty)?;
            let ptr = cx.allocas[dst];
            builder
                .build_store(ptr, val)
                .map_err(|e| CodegenError::Builder(e.to_string()))?;
            Ok(())
        }
        MirStmt::AssignField {
            dst,
            field_idx,
            value,
        } => {
            let base_ty = cx.f.locals[dst.0 as usize].ty;
            let layout = aggregate_layout(base_ty, cx.prog, cx.ptr_bytes).ok_or_else(|| {
                CodegenError::Builder(format!(
                    "AssignField on non-aggregate dst {:?} ty {:?}",
                    dst, base_ty
                ))
            })?;
            let field_ty = aggregate_field_ty(base_ty, *field_idx, cx.prog);
            let val = lower_rvalue(builder, cx, value, field_ty)?;
            let base_ptr = cx.allocas[dst];
            let field_ptr = field_addr(builder, cx, base_ptr, layout.offsets[*field_idx as usize])?;
            builder.build_store(field_ptr, val).map_err(be)?;
            Ok(())
        }
    }
}

/// Compute the address of a field at a given byte `offset` from
/// `base_ptr`. With opaque pointers, GEPing through `i8` lets us pick
/// any byte boundary without first declaring the aggregate's struct
/// type to LLVM.
fn field_addr<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    cx: &LoweringCx<'ctx, '_>,
    base_ptr: PointerValue<'ctx>,
    offset: u32,
) -> Result<PointerValue<'ctx>, CodegenError> {
    let off = cx.context.i64_type().const_int(offset as u64, false);
    let ptr = unsafe {
        builder
            .build_in_bounds_gep(cx.context.i8_type(), base_ptr, &[off], "field")
            .map_err(be)?
    };
    Ok(ptr)
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
        Rvalue::Field {
            base,
            field_idx,
            field_ty,
        } => {
            let base_ty = cx.f.locals[base.0 as usize].ty;
            let layout = aggregate_layout(base_ty, cx.prog, cx.ptr_bytes).ok_or_else(|| {
                CodegenError::Builder(format!(
                    "Rvalue::Field on non-aggregate base {:?} ty {:?}",
                    base, base_ty
                ))
            })?;
            let base_ptr = cx.allocas[base];
            let field_ptr = field_addr(builder, cx, base_ptr, layout.offsets[*field_idx as usize])?;
            let lty = llvm_basic_type(cx.context, *field_ty).ok_or_else(|| {
                CodegenError::Unsupported(format!(
                    "Rvalue::Field with field_ty {:?} — no scalar LLVM type",
                    field_ty
                ))
            })?;
            builder.build_load(lty, field_ptr, "fieldload").map_err(be)
        }
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
        Operand::Const(c) => emit_const(builder, cx, c, ty),
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
    cx: &LoweringCx<'ctx, '_>,
    c: &Const,
    ty: Ty,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let context = cx.context;
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
        Const::DataAddr(id) => {
            // The global was declared in pass 1b. Its `as_pointer_value`
            // is the constant address LLVM will emit at the use site.
            let global = cx.string_globals.get(id.0 as usize).ok_or_else(|| {
                CodegenError::Builder(format!(
                    "Const::DataAddr({}) but only {} string globals declared",
                    id.0,
                    cx.string_globals.len()
                ))
            })?;
            Ok(global.as_pointer_value().into())
        }
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
            if is_aggregate_ty(cx.f.return_ty) {
                // Aggregate return: memcpy the result slot into the
                // hidden out-pointer the caller passed in, then return
                // void. `op` is `Operand::Local(src)` for legal MIR.
                let src_local = match op {
                    Operand::Local(l) => l,
                    _ => {
                        return Err(CodegenError::Builder(
                            "aggregate Return with non-Local operand".into(),
                        ));
                    }
                };
                let layout = aggregate_layout(cx.f.return_ty, cx.prog, cx.ptr_bytes)
                    .ok_or_else(|| CodegenError::Builder("aggregate Return: no layout".into()))?;
                let out_ptr = cx.ret_out_ptr.ok_or_else(|| {
                    CodegenError::Builder("aggregate-returning fn has no ret_out_ptr".into())
                })?;
                let src_ptr = cx.allocas[src_local];
                let size = cx.context.i64_type().const_int(layout.size as u64, false);
                builder
                    .build_memcpy(
                        out_ptr,
                        layout.align.max(1),
                        src_ptr,
                        layout.align.max(1),
                        size,
                    )
                    .map_err(|e| CodegenError::Builder(format!("memcpy return: {e}")))?;
                builder.build_return(None).map_err(be)?;
            } else if matches!(cx.f.return_ty, Ty::U0 | Ty::Error) {
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
    let callee_returns_aggregate = is_aggregate_ty(callee_fn.return_ty);

    let mut arg_vals: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len() + 1);
    // Hidden out-pointer for aggregate return goes first; the dst local
    // already has its alloca allocated.
    if callee_returns_aggregate {
        let dst_ptr = cx.allocas[&dst];
        arg_vals.push(dst_ptr.into());
    }
    for (i, op) in args.iter().enumerate() {
        let param_local = callee_fn.params[i];
        let param_ty = callee_fn.locals[param_local.0 as usize].ty;
        if is_aggregate_ty(param_ty) {
            // Aggregate args pass the address of the source slot.
            // Aggregates can't be const-folded; the operand is always
            // `Operand::Local(src)` for legal MIR.
            let src_local = match op {
                Operand::Local(l) => l,
                _ => {
                    return Err(CodegenError::Builder(
                        "aggregate-typed call arg with non-Local operand".into(),
                    ));
                }
            };
            let src_ptr = cx.allocas[src_local];
            arg_vals.push(src_ptr.into());
        } else {
            let v = read_operand(builder, cx, op, param_ty)?;
            arg_vals.push(v.into());
        }
    }

    let call_site = builder
        .build_call(callee_value, &arg_vals, "call")
        .map_err(be)?;

    let dst_ty = cx.f.locals[dst.0 as usize].ty;
    if callee_returns_aggregate {
        // The result already landed in `dst`'s alloca via the hidden
        // out-pointer we passed in.
    } else if !matches!(dst_ty, Ty::U0 | Ty::Error) {
        // Scalar return: store into the dst local's alloca.
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
    let ptr_ty = context.ptr_type(AddressSpace::default());
    let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::with_capacity(f.params.len() + 1);
    // Hidden out-pointer prepended for aggregate returns (HANDOFF #21).
    if is_aggregate_ty(f.return_ty) {
        params.push(ptr_ty.into());
    }
    for &param_local in &f.params {
        let pty = f.locals[param_local.0 as usize].ty;
        if is_aggregate_ty(pty) {
            params.push(ptr_ty.into());
            continue;
        }
        let lty = llvm_basic_type(context, pty).ok_or_else(|| {
            CodegenError::Unsupported(format!(
                "fn `{}` param {:?} type {:?}; B.4 supports integers, bool, floats, classes, and slices",
                f.name, param_local, pty
            ))
        })?;
        params.push(lty.into());
    }
    Ok(match f.return_ty {
        // Aggregate returns flow through the hidden out-pointer; the
        // LLVM-level fn returns void.
        Ty::Class(_) | Ty::Slice(_) => context.void_type().fn_type(&params, false),
        Ty::U0 => context.void_type().fn_type(&params, false),
        Ty::Int(int_ty) => llvm_int_type(context, int_ty).fn_type(&params, false),
        Ty::Bool => context.bool_type().fn_type(&params, false),
        Ty::Float(float_ty) => llvm_float_type(context, float_ty).fn_type(&params, false),
        ref other => {
            return Err(CodegenError::Unsupported(format!(
                "fn `{}` return type {:?}; B.4 supports `u0`, integers, bool, floats, classes, and slices",
                f.name, other
            )));
        }
    })
}

/// Map a [`Ty`] to its LLVM `BasicTypeEnum` for primitive scalar use
/// (alloca / load / store / fn signature). Returns `None` for `u0`
/// (no value), aggregates (those use `[N x i8]` allocas + `ptr` in
/// signatures, not a basic scalar type), and unsupported types.
fn llvm_basic_type<'ctx>(context: &'ctx Context, ty: Ty) -> Option<BasicTypeEnum<'ctx>> {
    match ty {
        Ty::U0 => None,
        Ty::Bool => Some(context.bool_type().into()),
        Ty::Int(int_ty) => Some(llvm_int_type(context, int_ty).into()),
        Ty::Float(float_ty) => Some(llvm_float_type(context, float_ty).into()),
        // `*T` raw pointers (FFI-restricted at typeck per HANDOFF #13;
        // also the type of `slice.data`) lower as opaque `ptr`. With
        // LLVM ≥ 15's opaque pointers the pointee type isn't part of
        // the value, so `*u8` and `*i8` etc. are all the same `ptr`.
        Ty::Ptr(_) => Some(context.ptr_type(AddressSpace::default()).into()),
        // Aggregates: routed through the by-pointer ABI / `[N x i8]`
        // allocas; `llvm_basic_type` is for *scalar* type lookup only.
        Ty::Class(_) | Ty::Slice(_) => None,
        Ty::Rune | Ty::Error => None,
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
