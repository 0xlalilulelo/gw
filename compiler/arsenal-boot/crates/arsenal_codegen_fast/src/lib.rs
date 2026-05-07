//! GW fast backend.
//!
//! Phase 1 of the compiler uses Cranelift as a placeholder backend; the
//! TPDE-style template encoder described in `docs/architecture.md`
//! Part F.1 lands in Phase 7. The crate name `arsenal_codegen_fast`
//! refers to its eventual role; the current Cranelift implementation
//! satisfies the same `MirProgram → object bytes` contract that the
//! TPDE backend will inherit.
//!
//! Public entry point: [`compile_program`].

use arsenal_mir::{
    BinOp, BlockId, CastKind, Const, Local, MirBlock, MirFn, MirProgram, MirStmt, Operand, Rvalue,
    Terminator, UnOp,
};
use arsenal_typeck::{ClassLayout, FloatTy, IntTy, Ty};
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, StackSlot};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::{isa, Context};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use rustc_hash::FxHashMap;
use std::sync::Arc;
use target_lexicon::Triple;

/// Codegen error.
#[derive(Debug)]
pub enum CodegenError {
    /// Failed to resolve a target ISA. The contained string is the
    /// underlying error message from `cranelift_codegen::isa::lookup`
    /// or `Flags::set`.
    IsaLookup(String),
    /// Cranelift module error during declare/define.
    Module(String),
    /// Cranelift codegen error during finalize/emit.
    Codegen(String),
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IsaLookup(s) => write!(f, "isa lookup failed: {s}"),
            Self::Module(s) => write!(f, "cranelift module error: {s}"),
            Self::Codegen(s) => write!(f, "cranelift codegen error: {s}"),
        }
    }
}

impl std::error::Error for CodegenError {}

/// Compile a [`MirProgram`] to an object file targeting `triple`.
///
/// Returns the raw bytes of the resulting object file (ELF on Linux,
/// Mach-O on macOS, COFF on Windows). The caller is responsible for
/// linking; see `arsenal_driver` for the `cc`-based linker invocation.
pub fn compile_program(
    prog: &MirProgram,
    triple: Triple,
    object_name: &str,
) -> Result<Vec<u8>, CodegenError> {
    // ISA setup. We use the default flags ("opt_level=none") for fast
    // builds; release-mode tuning is a Phase 8+ concern.
    let mut flag_builder = settings::builder();
    flag_builder
        .set("use_colocated_libcalls", "false")
        .map_err(|e| CodegenError::IsaLookup(e.to_string()))?;
    flag_builder
        .set("is_pic", "true")
        .map_err(|e| CodegenError::IsaLookup(e.to_string()))?;
    let flags = settings::Flags::new(flag_builder);

    let isa_builder = isa::lookup(triple).map_err(|e| CodegenError::IsaLookup(e.to_string()))?;
    let isa = isa_builder
        .finish(flags)
        .map_err(|e| CodegenError::IsaLookup(e.to_string()))?;

    let object_builder = ObjectBuilder::new(
        Arc::clone(&isa),
        object_name.as_bytes().to_vec(),
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| CodegenError::Module(e.to_string()))?;
    let mut module = ObjectModule::new(object_builder);

    // Pass 1: declare every function so calls can resolve forward.
    // `extern fn` declarations get `Linkage::Import` so the system
    // linker resolves them against another translation unit (libc,
    // typically). Locally-defined functions get `Linkage::Export` so
    // the entry point (`main`) is reachable.
    let mut fn_ids: Vec<cranelift_module::FuncId> = Vec::with_capacity(prog.functions.len());
    for f in &prog.functions {
        let sig = make_signature(&module, f);
        let linkage = if f.is_extern {
            Linkage::Import
        } else {
            Linkage::Export
        };
        let id = module
            .declare_function(&f.name, linkage, &sig)
            .map_err(|e| CodegenError::Module(e.to_string()))?;
        fn_ids.push(id);
    }

    // Pass 1b: declare and define one read-only data object per string
    // literal referenced by the MIR. The data lives in `.rodata`; each
    // `Const::DataAddr(id)` lowers to `global_value` of the matching
    // `DataId`. Names are synthetic (`__gw_str_<i>`) and not exported.
    let mut data_ids: Vec<DataId> = Vec::with_capacity(prog.string_literals.len());
    for (i, bytes) in prog.string_literals.iter().enumerate() {
        let name = format!("__gw_str_{i}");
        let id = module
            .declare_data(
                &name,
                Linkage::Local,
                /* writable */ false,
                /* tls */ false,
            )
            .map_err(|e| CodegenError::Module(e.to_string()))?;
        let mut desc = DataDescription::new();
        // Cranelift rejects empty data definitions; pad zero-length
        // literals to a single byte so the symbol still resolves. The
        // GW-level length stays 0 because that comes from the slice's
        // len field, not the data object's size.
        let payload: Vec<u8> = if bytes.is_empty() {
            vec![0]
        } else {
            bytes.clone()
        };
        desc.define(payload.into_boxed_slice());
        module
            .define_data(id, &desc)
            .map_err(|e| CodegenError::Module(e.to_string()))?;
        data_ids.push(id);
    }

    // Pass 2: define each non-extern function.
    let mut ctx = module.make_context();
    let mut fbctx = FunctionBuilderContext::new();
    for (i, f) in prog.functions.iter().enumerate() {
        if f.is_extern {
            continue;
        }
        ctx.func.signature = make_signature(&module, f);
        define_fn(
            &mut ctx,
            &mut fbctx,
            &mut module,
            &fn_ids,
            &data_ids,
            prog,
            f,
        )?;
        module
            .define_function(fn_ids[i], &mut ctx)
            .map_err(|e| CodegenError::Module(e.to_string()))?;
        module.clear_context(&mut ctx);
    }

    // Finalise the object.
    let object = module.finish();
    object
        .emit()
        .map_err(|e| CodegenError::Codegen(e.to_string()))
}

/// Computed layout for a class type: total byte size, alignment, and
/// the byte offset of each field. Field `i` lives at
/// `offsets[i]..offsets[i] + size_of(fields[i].ty)`.
struct ResolvedClassLayout {
    size: u32,
    align: u32,
    offsets: Vec<u32>,
}

/// Compute storage layout for a class. Phase 1 supports only primitive
/// fields (typeck rejects nested classes, so this won't recurse).
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
    let size = if max_align == 0 {
        offset
    } else {
        align_up(offset, max_align)
    };
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
        // pointer-sized just so codegen doesn't divide by zero.
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

fn make_signature(module: &ObjectModule, f: &MirFn) -> ir::Signature {
    let mut sig = module.make_signature();
    for &param_local in &f.params {
        let ty = f.locals[param_local.0 as usize].ty;
        if let Some(clt) = clif_ty(ty, module) {
            sig.params.push(AbiParam::new(clt));
        }
    }
    if let Some(clt) = clif_ty(f.return_ty, module) {
        sig.returns.push(AbiParam::new(clt));
    }
    sig
}

fn clif_ty(ty: Ty, module: &ObjectModule) -> Option<ir::Type> {
    let ptr_bits = module.target_config().pointer_bits() as u32;
    let ptr_clt = match ptr_bits {
        64 => ir::types::I64,
        32 => ir::types::I32,
        _ => ir::types::I64,
    };
    Some(match ty {
        Ty::U0 => return None,
        Ty::Bool => ir::types::I8,
        Ty::Int(IntTy::I8) | Ty::Int(IntTy::U8) => ir::types::I8,
        Ty::Int(IntTy::I16) | Ty::Int(IntTy::U16) => ir::types::I16,
        Ty::Int(IntTy::I32) | Ty::Int(IntTy::U32) => ir::types::I32,
        Ty::Int(IntTy::I64) | Ty::Int(IntTy::U64) => ir::types::I64,
        Ty::Int(IntTy::ISize) | Ty::Int(IntTy::USize) => ptr_clt,
        Ty::Float(FloatTy::F32) => ir::types::F32,
        Ty::Float(FloatTy::F64) => ir::types::F64,
        Ty::Rune => ir::types::I32,
        Ty::Ptr(_) => ptr_clt,
        Ty::Error => ir::types::I32,
        // Phase 1 doesn't model class- or slice-typed scalars (those are
        // stack-slotted by the time they reach a `read_operand` call);
        // if anything still slips through, fall back to pointer-width.
        _ => ptr_clt,
    })
}

fn define_fn(
    ctx: &mut Context,
    fbctx: &mut FunctionBuilderContext,
    module: &mut ObjectModule,
    fn_ids: &[cranelift_module::FuncId],
    data_ids: &[DataId],
    prog: &MirProgram,
    f: &MirFn,
) -> Result<(), CodegenError> {
    let mut builder = FunctionBuilder::new(&mut ctx.func, fbctx);
    let ptr_bytes = module.target_config().pointer_bytes() as u32;

    // Locals are allocated either as a Cranelift Variable (primitives,
    // bool, rune) or as a StackSlot (class- and slice-typed locals).
    // Variables get SSA-style def/use; StackSlots get stack_load /
    // stack_store.
    let mut local_var: FxHashMap<Local, Variable> = FxHashMap::default();
    let mut local_slot: FxHashMap<Local, StackSlot> = FxHashMap::default();
    for (i, decl) in f.locals.iter().enumerate() {
        let local = Local(i as u32);
        match decl.ty {
            Ty::Class(_) | Ty::Slice(_) => {
                let layout =
                    aggregate_layout(decl.ty, prog, ptr_bytes).unwrap_or(ResolvedClassLayout {
                        size: ptr_bytes,
                        align: ptr_bytes,
                        offsets: Vec::new(),
                    });
                let slot = builder.create_sized_stack_slot(ir::StackSlotData::new(
                    ir::StackSlotKind::ExplicitSlot,
                    layout.size.max(1),
                    layout.align.trailing_zeros() as u8,
                ));
                local_slot.insert(local, slot);
            }
            _ => {
                let var = Variable::from_u32(i as u32);
                if let Some(clt) = clif_ty(decl.ty, module) {
                    builder.declare_var(var, clt);
                } else {
                    // Placeholder for unit-typed locals so indices align.
                    builder.declare_var(var, ir::types::I8);
                }
                local_var.insert(local, var);
            }
        }
    }

    // Pre-declare each program-level data object inside this function
    // so `Const::DataAddr` lowers to `ins.global_value` against a cached
    // GlobalValue rather than needing `&mut module` deep in the rvalue
    // path.
    let mut data_gvs: Vec<ir::GlobalValue> = Vec::with_capacity(data_ids.len());
    for &did in data_ids {
        let gv = module.declare_data_in_func(did, builder.func);
        data_gvs.push(gv);
    }

    // Allocate one Cranelift Block per MIR block.
    let mut clif_block: FxHashMap<BlockId, ir::Block> = FxHashMap::default();
    for i in 0..f.blocks.len() {
        let bb = builder.create_block();
        clif_block.insert(BlockId(i as u32), bb);
    }

    // Append parameter values to the entry block and assign them to the
    // parameter Variables. (Class-typed parameters are forbidden by
    // typeck in Phase 1.)
    let entry = clif_block[&BlockId(0)];
    builder.switch_to_block(entry);
    for (i, &param_local) in f.params.iter().enumerate() {
        let ty = f.locals[param_local.0 as usize].ty;
        let Some(clt) = clif_ty(ty, module) else {
            continue;
        };
        builder.append_block_param(entry, clt);
        let v = builder.block_params(entry)[i];
        if let Some(var) = local_var.get(&param_local) {
            builder.def_var(*var, v);
        }
    }

    // Lower each block. Cranelift requires every block to be sealed
    // after all predecessors are known; the simplest approach for a
    // first pass is to seal_all_blocks at the end.
    let cx = LoweringCx {
        prog,
        ptr_bytes,
        local_var: &local_var,
        local_slot: &local_slot,
        clif_block: &clif_block,
        fn_ids,
        data_gvs: &data_gvs,
    };
    for (i, mir_block) in f.blocks.iter().enumerate() {
        let bb = clif_block[&BlockId(i as u32)];
        builder.switch_to_block(bb);
        lower_block(&mut builder, module, f, mir_block, &cx);
    }

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

/// Bag of state passed down through codegen lowering. Lets us avoid
/// threading half a dozen `&` references through every helper.
struct LoweringCx<'a> {
    prog: &'a MirProgram,
    ptr_bytes: u32,
    local_var: &'a FxHashMap<Local, Variable>,
    local_slot: &'a FxHashMap<Local, StackSlot>,
    clif_block: &'a FxHashMap<BlockId, ir::Block>,
    fn_ids: &'a [cranelift_module::FuncId],
    /// One [`ir::GlobalValue`] per [`StringLitId`], pre-declared in this
    /// function's scope so `Const::DataAddr` lowers without `&mut module`.
    data_gvs: &'a [ir::GlobalValue],
}

impl<'a> LoweringCx<'a> {
    fn aggregate_layout(&self, ty: Ty) -> Option<ResolvedClassLayout> {
        aggregate_layout(ty, self.prog, self.ptr_bytes)
    }

    fn ptr_ty(&self) -> ir::Type {
        match self.ptr_bytes {
            8 => ir::types::I64,
            4 => ir::types::I32,
            _ => ir::types::I64,
        }
    }
}

/// Compute the offset/size/align layout for an aggregate-typed local
/// (class or slice). Slices have a fixed two-field shape: data pointer
/// at offset 0, `len: usize` at offset `ptr_bytes`. Classes consult the
/// `class_layouts` map populated by typeck.
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
/// codegen to pick the load/store width. For slices, both fields are
/// pointer-sized in Phase 1; reporting `usize` for the data pointer is
/// a small lie that yields the correct codegen width and is invisible
/// at the surface (slice `.data` access is not exposed yet).
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

fn lower_block(
    fb: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    f: &MirFn,
    block: &MirBlock,
    cx: &LoweringCx<'_>,
) {
    for stmt in &block.statements {
        match stmt {
            MirStmt::Assign { dst, value } => {
                lower_assign_stmt(fb, module, f, *dst, value, cx);
            }
            MirStmt::AssignField {
                dst,
                field_idx,
                value,
            } => {
                let dst_ty = f.locals[dst.0 as usize].ty;
                let layout = match cx.aggregate_layout(dst_ty) {
                    Some(l) => l,
                    None => continue,
                };
                let field_ty = aggregate_field_ty(dst_ty, *field_idx, cx.prog);
                let offset = layout.offsets[*field_idx as usize];
                let want = clif_ty(field_ty, module).unwrap_or(cx.ptr_ty());
                let val = lower_rvalue(fb, module, f, value, cx, want);
                let slot = cx.local_slot[dst];
                fb.ins().stack_store(val, slot, offset as i32);
            }
        }
    }
    match &block.terminator {
        Terminator::Goto(target) => {
            fb.ins().jump(cx.clif_block[target], &[]);
        }
        Terminator::Branch {
            cond,
            then_bb,
            else_bb,
        } => {
            let c = read_operand(fb, f, cond, cx, ir::types::I8);
            // Cranelift's brif takes a boolean / integer condition; any
            // non-zero is true.
            fb.ins()
                .brif(c, cx.clif_block[then_bb], &[], cx.clif_block[else_bb], &[]);
        }
        Terminator::Return(op) => {
            if matches!(f.return_ty, Ty::U0 | Ty::Error) {
                fb.ins().return_(&[]);
            } else {
                let want = clif_ty(f.return_ty, module).unwrap_or(ir::types::I32);
                let v = read_operand(fb, f, op, cx, want);
                fb.ins().return_(&[v]);
            }
        }
        Terminator::Call {
            callee,
            args,
            dst,
            target_bb,
        } => {
            let func_ref = module.declare_func_in_func(cx.fn_ids[callee.0 as usize], fb.func);
            let mut arg_vals = Vec::with_capacity(args.len());
            let sig_ref = fb.func.dfg.ext_funcs[func_ref].signature;
            for (i, op) in args.iter().enumerate() {
                let want = fb.func.dfg.signatures[sig_ref].params[i].value_type;
                arg_vals.push(read_operand(fb, f, op, cx, want));
            }
            let inst = fb.ins().call(func_ref, &arg_vals);
            let results = fb.inst_results(inst).to_vec();
            if !results.is_empty() {
                if let Some(var) = cx.local_var.get(dst) {
                    fb.def_var(*var, results[0]);
                }
            }
            fb.ins().jump(cx.clif_block[target_bb], &[]);
        }
        Terminator::Unreachable => {
            fb.ins().trap(ir::TrapCode::user(1).expect("trap code"));
        }
    }
}

/// Lower a `MirStmt::Assign { dst, value }`. Branches on whether the
/// destination local is primitive-typed (Variable + def_var) or
/// aggregate-typed (StackSlot + per-field copy). "Aggregate" covers
/// both classes and slices in Phase 1.
fn lower_assign_stmt(
    fb: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    f: &MirFn,
    dst: Local,
    value: &Rvalue,
    cx: &LoweringCx<'_>,
) {
    let dst_ty = f.locals[dst.0 as usize].ty;
    if matches!(dst_ty, Ty::Class(_) | Ty::Slice(_)) {
        // Aggregate-typed destination. Phase 1 only produces such
        // Assigns via let-init shadowing of a struct/string literal
        // temp: `let p = Foo {...};` or `let s: []u8 = "...";` allocates
        // a temp inside the lit lowering, emits per-field AssignFields,
        // and then Assigns the temp into the let's local. Lower as
        // field-by-field memcpy from src slot to dst slot.
        let Rvalue::Use(Operand::Local(src)) = value else {
            // Other rvalue kinds for aggregate dst are unreachable in
            // Phase 1; play it safe and emit a trap.
            fb.ins().trap(ir::TrapCode::user(3).expect("trap code"));
            return;
        };
        let src_slot = match cx.local_slot.get(src) {
            Some(s) => *s,
            None => return,
        };
        let dst_slot = match cx.local_slot.get(&dst) {
            Some(s) => *s,
            None => return,
        };
        let layout = match cx.aggregate_layout(dst_ty) {
            Some(l) => l,
            None => return,
        };
        for (i, &off) in layout.offsets.iter().enumerate() {
            let field_ty = aggregate_field_ty(dst_ty, i as u32, cx.prog);
            let Some(ty) = clif_ty(field_ty, module) else {
                continue;
            };
            let v = fb.ins().stack_load(ty, src_slot, off as i32);
            fb.ins().stack_store(v, dst_slot, off as i32);
        }
        return;
    }
    // Primitive dst.
    let want = clif_ty(dst_ty, module).unwrap_or(ir::types::I32);
    let val = lower_rvalue(fb, module, f, value, cx, want);
    if let Some(var) = cx.local_var.get(&dst) {
        fb.def_var(*var, val);
    }
}

fn lower_rvalue(
    fb: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    f: &MirFn,
    rv: &Rvalue,
    cx: &LoweringCx<'_>,
    want_ty: ir::Type,
) -> ir::Value {
    match rv {
        Rvalue::Use(op) => read_operand(fb, f, op, cx, want_ty),
        Rvalue::BinOp { op, lhs, rhs, ty } => {
            let clt = clif_ty(*ty, module).unwrap_or(ir::types::I32);
            let l = read_operand(fb, f, lhs, cx, clt);
            let r = read_operand(fb, f, rhs, cx, clt);
            lower_binop(fb, *op, *ty, l, r)
        }
        Rvalue::UnOp { op, operand, ty } => {
            let clt = clif_ty(*ty, module).unwrap_or(ir::types::I32);
            let v = read_operand(fb, f, operand, cx, clt);
            lower_unop(fb, *op, *ty, v)
        }
        Rvalue::Field {
            base,
            field_idx,
            field_ty,
        } => {
            let base_ty = f.locals[base.0 as usize].ty;
            let layout = match cx.aggregate_layout(base_ty) {
                Some(l) => l,
                None => return fb.ins().iconst(want_ty, 0),
            };
            let offset = layout.offsets[*field_idx as usize] as i32;
            let slot = cx.local_slot[base];
            let load_ty = clif_ty(*field_ty, module).unwrap_or(want_ty);
            fb.ins().stack_load(load_ty, slot, offset)
        }
        Rvalue::Cast {
            kind,
            operand,
            src_ty,
            dst_ty,
        } => {
            // Read the operand at the *source* clif type so
            // sextend/uextend/ireduce see the original width. `clif_ty`
            // returns I32 as the safe default for non-primitives, but
            // typeck restricts A.1 cast operands to integers, so the
            // unwrap_or branch is unreachable on legal input.
            let src_clif = clif_ty(*src_ty, module).unwrap_or(ir::types::I32);
            let dst_clif = clif_ty(*dst_ty, module).unwrap_or(want_ty);
            let v = read_operand(fb, f, operand, cx, src_clif);
            match kind {
                CastKind::IntWiden { signed: true } => fb.ins().sextend(dst_clif, v),
                CastKind::IntWiden { signed: false } => fb.ins().uextend(dst_clif, v),
                CastKind::IntTrunc => fb.ins().ireduce(dst_clif, v),
                // Same width, signedness reinterpret: Cranelift integer
                // types are unsigned-by-bit-pattern, so the operand
                // value is already correct.
                CastKind::IntBitcast => v,
            }
        }
    }
}

fn read_operand(
    fb: &mut FunctionBuilder<'_>,
    f: &MirFn,
    op: &Operand,
    cx: &LoweringCx<'_>,
    want: ir::Type,
) -> ir::Value {
    match op {
        Operand::Const(c) => emit_const(fb, c, want, cx),
        Operand::Local(l) => {
            // Aggregate-typed locals (class, slice) can't be "read" as a
            // single value. Phase 1 paths that try to are caught by
            // `lower_assign_stmt`'s aggregate-dst branch and don't reach
            // here. For primitives, use_var.
            if let Some(var) = cx.local_var.get(l) {
                fb.use_var(*var)
            } else {
                // Fallback so codegen stays sound on unexpected paths.
                let _ = f;
                let _ = MemFlags::new();
                fb.ins().iconst(want, 0)
            }
        }
    }
}

fn emit_const(
    fb: &mut FunctionBuilder<'_>,
    c: &Const,
    want: ir::Type,
    cx: &LoweringCx<'_>,
) -> ir::Value {
    match c {
        Const::Int { value, ty } => {
            let clt = match ty {
                IntTy::I8 | IntTy::U8 => ir::types::I8,
                IntTy::I16 | IntTy::U16 => ir::types::I16,
                IntTy::I32 | IntTy::U32 => ir::types::I32,
                IntTy::I64 | IntTy::U64 => ir::types::I64,
                IntTy::ISize | IntTy::USize => ir::types::I64,
            };
            // Sign-extend the i128 literal value into i64 for Cranelift.
            // Cranelift's iconst takes an i64 and reinterprets bits.
            let bits: i64 = *value as i64;
            fb.ins().iconst(clt, bits)
        }
        Const::Bool(b) => fb.ins().iconst(ir::types::I8, if *b { 1 } else { 0 }),
        Const::Float { bits, ty } => match ty {
            FloatTy::F32 => fb.ins().f32const(f32::from_bits(*bits as u32)),
            FloatTy::F64 => fb.ins().f64const(f64::from_bits(*bits)),
        },
        Const::DataAddr(id) => {
            // Materialise the rodata payload's address as a pointer-
            // sized value via the pre-declared GlobalValue cached in
            // `cx.data_gvs`.
            let gv = cx.data_gvs[id.0 as usize];
            fb.ins().global_value(cx.ptr_ty(), gv)
        }
        Const::Unit | Const::Error => fb.ins().iconst(want, 0),
    }
}

fn lower_binop(
    fb: &mut FunctionBuilder<'_>,
    op: BinOp,
    ty: Ty,
    l: ir::Value,
    r: ir::Value,
) -> ir::Value {
    let signed = ty.is_signed_int();
    let is_float = ty.is_float();
    match op {
        BinOp::Add => {
            if is_float {
                fb.ins().fadd(l, r)
            } else {
                fb.ins().iadd(l, r)
            }
        }
        BinOp::Sub => {
            if is_float {
                fb.ins().fsub(l, r)
            } else {
                fb.ins().isub(l, r)
            }
        }
        BinOp::Mul => {
            if is_float {
                fb.ins().fmul(l, r)
            } else {
                fb.ins().imul(l, r)
            }
        }
        BinOp::Div => {
            if is_float {
                fb.ins().fdiv(l, r)
            } else if signed {
                fb.ins().sdiv(l, r)
            } else {
                fb.ins().udiv(l, r)
            }
        }
        BinOp::Mod => {
            if signed {
                fb.ins().srem(l, r)
            } else {
                fb.ins().urem(l, r)
            }
        }
        BinOp::Pow => {
            // No native Cranelift integer pow; emit a runtime-safe
            // trap until we add a real implementation in a later
            // increment.
            fb.ins().trap(ir::TrapCode::user(2).expect("trap code"));
            fb.ins().iconst(ir::types::I32, 0)
        }
        BinOp::BitAnd => fb.ins().band(l, r),
        BinOp::BitOr => fb.ins().bor(l, r),
        BinOp::BitXor => fb.ins().bxor(l, r),
        BinOp::Shl => fb.ins().ishl(l, r),
        BinOp::Shr => {
            if signed {
                fb.ins().sshr(l, r)
            } else {
                fb.ins().ushr(l, r)
            }
        }
        BinOp::LogAnd => fb.ins().band(l, r),
        BinOp::LogOr => fb.ins().bor(l, r),
        BinOp::Eq => {
            if is_float {
                fb.ins().fcmp(ir::condcodes::FloatCC::Equal, l, r)
            } else {
                fb.ins().icmp(ir::condcodes::IntCC::Equal, l, r)
            }
        }
        BinOp::Ne => {
            if is_float {
                fb.ins().fcmp(ir::condcodes::FloatCC::NotEqual, l, r)
            } else {
                fb.ins().icmp(ir::condcodes::IntCC::NotEqual, l, r)
            }
        }
        BinOp::Lt => {
            if is_float {
                fb.ins().fcmp(ir::condcodes::FloatCC::LessThan, l, r)
            } else {
                fb.ins().icmp(
                    if signed {
                        ir::condcodes::IntCC::SignedLessThan
                    } else {
                        ir::condcodes::IntCC::UnsignedLessThan
                    },
                    l,
                    r,
                )
            }
        }
        BinOp::Le => {
            if is_float {
                fb.ins().fcmp(ir::condcodes::FloatCC::LessThanOrEqual, l, r)
            } else {
                fb.ins().icmp(
                    if signed {
                        ir::condcodes::IntCC::SignedLessThanOrEqual
                    } else {
                        ir::condcodes::IntCC::UnsignedLessThanOrEqual
                    },
                    l,
                    r,
                )
            }
        }
        BinOp::Gt => {
            if is_float {
                fb.ins().fcmp(ir::condcodes::FloatCC::GreaterThan, l, r)
            } else {
                fb.ins().icmp(
                    if signed {
                        ir::condcodes::IntCC::SignedGreaterThan
                    } else {
                        ir::condcodes::IntCC::UnsignedGreaterThan
                    },
                    l,
                    r,
                )
            }
        }
        BinOp::Ge => {
            if is_float {
                fb.ins()
                    .fcmp(ir::condcodes::FloatCC::GreaterThanOrEqual, l, r)
            } else {
                fb.ins().icmp(
                    if signed {
                        ir::condcodes::IntCC::SignedGreaterThanOrEqual
                    } else {
                        ir::condcodes::IntCC::UnsignedGreaterThanOrEqual
                    },
                    l,
                    r,
                )
            }
        }
    }
}

fn lower_unop(fb: &mut FunctionBuilder<'_>, op: UnOp, ty: Ty, v: ir::Value) -> ir::Value {
    match op {
        UnOp::Neg => {
            if ty.is_float() {
                fb.ins().fneg(v)
            } else {
                fb.ins().ineg(v)
            }
        }
        UnOp::Not => {
            // Logical not on a bool: XOR with 1.
            let one = fb.ins().iconst(ir::types::I8, 1);
            fb.ins().bxor(v, one)
        }
        UnOp::BitNot => fb.ins().bnot(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_codegen::ir::{Function, Opcode, Signature, UserFuncName};
    use cranelift_codegen::isa::CallConv;

    #[test]
    fn host_triple_resolves() {
        // Smoke test: the host triple should be lookup-able. This
        // verifies cranelift's ISA detection works on the dev box.
        let _isa = isa::lookup(Triple::host()).expect("host isa");
    }

    /// Build a single-block function, run [`lower_binop`] inside it,
    /// and return every opcode it emitted.
    fn opcodes_for_binop(op: BinOp, ty: Ty, operand_ty: ir::Type) -> Vec<Opcode> {
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(operand_ty));
        sig.params.push(AbiParam::new(operand_ty));
        sig.returns.push(AbiParam::new(ir::types::I8));
        let mut func = Function::with_name_signature(UserFuncName::user(0, 0), sig);
        let mut fbctx = FunctionBuilderContext::new();
        let mut fb = FunctionBuilder::new(&mut func, &mut fbctx);
        let block = fb.create_block();
        fb.append_block_params_for_function_params(block);
        fb.switch_to_block(block);
        fb.seal_block(block);
        let l = fb.block_params(block)[0];
        let r = fb.block_params(block)[1];
        let v = lower_binop(&mut fb, op, ty, l, r);
        fb.ins().return_(&[v]);
        fb.finalize();
        let mut opcodes = Vec::new();
        for block in func.layout.blocks() {
            for inst in func.layout.block_insts(block) {
                opcodes.push(func.dfg.insts[inst].opcode());
            }
        }
        opcodes
    }

    /// Regression: comparison BinOps used to lower unconditionally to
    /// `icmp`, which Cranelift's verifier rejects when the operands
    /// are F32/F64. The fix branches on `ty.is_float()` and emits
    /// `fcmp` for floats.
    #[test]
    fn float_eq_lowers_to_fcmp_not_icmp() {
        let ops = opcodes_for_binop(BinOp::Eq, Ty::Float(FloatTy::F64), ir::types::F64);
        assert!(
            ops.contains(&Opcode::Fcmp),
            "expected Fcmp for float ==, saw {ops:?}"
        );
        assert!(
            !ops.contains(&Opcode::Icmp),
            "icmp leaked into a float comparison: {ops:?}"
        );
    }

    #[test]
    fn float_lt_le_gt_ge_ne_all_lower_to_fcmp() {
        for op in [BinOp::Ne, BinOp::Lt, BinOp::Le, BinOp::Gt, BinOp::Ge] {
            let ops = opcodes_for_binop(op, Ty::Float(FloatTy::F32), ir::types::F32);
            assert!(
                ops.contains(&Opcode::Fcmp),
                "expected Fcmp for float {op:?}, saw {ops:?}"
            );
            assert!(
                !ops.contains(&Opcode::Icmp),
                "icmp leaked into float {op:?}: {ops:?}"
            );
        }
    }

    #[test]
    fn integer_eq_still_lowers_to_icmp() {
        let ops = opcodes_for_binop(BinOp::Eq, Ty::Int(IntTy::I32), ir::types::I32);
        assert!(
            ops.contains(&Opcode::Icmp),
            "expected Icmp for int ==, saw {ops:?}"
        );
        assert!(
            !ops.contains(&Opcode::Fcmp),
            "fcmp leaked into int comparison: {ops:?}"
        );
    }
}
