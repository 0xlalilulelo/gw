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
    BinOp, BlockId, Const, Local, MirBlock, MirFn, MirProgram, MirStmt, Operand, Rvalue,
    Terminator, UnOp,
};
use arsenal_typeck::{FloatTy, IntTy, Ty};
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::{isa, Context};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{Linkage, Module};
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

    // Pass 2: define each non-extern function.
    let mut ctx = module.make_context();
    let mut fbctx = FunctionBuilderContext::new();
    for (i, f) in prog.functions.iter().enumerate() {
        if f.is_extern {
            continue;
        }
        ctx.func.signature = make_signature(&module, f);
        define_fn(&mut ctx, &mut fbctx, &mut module, &fn_ids, f)?;
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
    Some(match ty {
        Ty::U0 => return None,
        Ty::Bool => ir::types::I8,
        Ty::Int(IntTy::I8) | Ty::Int(IntTy::U8) => ir::types::I8,
        Ty::Int(IntTy::I16) | Ty::Int(IntTy::U16) => ir::types::I16,
        Ty::Int(IntTy::I32) | Ty::Int(IntTy::U32) => ir::types::I32,
        Ty::Int(IntTy::I64) | Ty::Int(IntTy::U64) => ir::types::I64,
        Ty::Int(IntTy::ISize) | Ty::Int(IntTy::USize) => match ptr_bits {
            64 => ir::types::I64,
            32 => ir::types::I32,
            _ => ir::types::I64,
        },
        Ty::Float(FloatTy::F32) => ir::types::F32,
        Ty::Float(FloatTy::F64) => ir::types::F64,
        Ty::Rune => ir::types::I32,
        Ty::Error => ir::types::I32,
        // Phase 1 doesn't model classes, slices, refs, etc. yet; if the
        // type checker hands us one, fall back to a pointer-width int
        // so codegen still produces something. Future Ty variants land
        // through this arm until they get explicit lowering rules.
        _ => match ptr_bits {
            64 => ir::types::I64,
            _ => ir::types::I32,
        },
    })
}

fn define_fn(
    ctx: &mut Context,
    fbctx: &mut FunctionBuilderContext,
    module: &mut ObjectModule,
    fn_ids: &[cranelift_module::FuncId],
    f: &MirFn,
) -> Result<(), CodegenError> {
    let mut builder = FunctionBuilder::new(&mut ctx.func, fbctx);

    // Allocate one Cranelift Variable per MIR Local.
    let mut local_var: FxHashMap<Local, Variable> = FxHashMap::default();
    for (i, decl) in f.locals.iter().enumerate() {
        let var = Variable::from_u32(i as u32);
        if let Some(clt) = clif_ty(decl.ty, module) {
            builder.declare_var(var, clt);
        } else {
            // Allocate a placeholder for unit-typed locals so indices
            // align; we never read these.
            builder.declare_var(var, ir::types::I8);
        }
        local_var.insert(Local(i as u32), var);
    }

    // Allocate one Cranelift Block per MIR block.
    let mut clif_block: FxHashMap<BlockId, ir::Block> = FxHashMap::default();
    for i in 0..f.blocks.len() {
        let bb = builder.create_block();
        clif_block.insert(BlockId(i as u32), bb);
    }

    // Append parameter values to the entry block and assign them to the
    // parameter Variables.
    let entry = clif_block[&BlockId(0)];
    builder.switch_to_block(entry);
    for (i, &param_local) in f.params.iter().enumerate() {
        let ty = f.locals[param_local.0 as usize].ty;
        let Some(clt) = clif_ty(ty, module) else {
            continue;
        };
        builder.append_block_param(entry, clt);
        let v = builder.block_params(entry)[i];
        builder.def_var(local_var[&param_local], v);
    }

    // Lower each block. Cranelift requires every block to be sealed
    // after all predecessors are known; the simplest approach for a
    // first pass is to seal_all_blocks at the end.
    for (i, mir_block) in f.blocks.iter().enumerate() {
        let bb = clif_block[&BlockId(i as u32)];
        builder.switch_to_block(bb);
        lower_block(
            &mut builder,
            module,
            f,
            fn_ids,
            mir_block,
            &local_var,
            &clif_block,
        );
    }

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

fn lower_block(
    fb: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    f: &MirFn,
    fn_ids: &[cranelift_module::FuncId],
    block: &MirBlock,
    local_var: &FxHashMap<Local, Variable>,
    clif_block: &FxHashMap<BlockId, ir::Block>,
) {
    for stmt in &block.statements {
        match stmt {
            MirStmt::Assign { dst, value } => {
                let val = lower_rvalue(fb, module, f, value, local_var);
                fb.def_var(local_var[dst], val);
            }
        }
    }
    match &block.terminator {
        Terminator::Goto(target) => {
            fb.ins().jump(clif_block[target], &[]);
        }
        Terminator::Branch {
            cond,
            then_bb,
            else_bb,
        } => {
            let c = read_operand(fb, f, cond, local_var, ir::types::I8);
            // Cranelift's brif takes a boolean / integer condition; any
            // non-zero is true.
            fb.ins()
                .brif(c, clif_block[then_bb], &[], clif_block[else_bb], &[]);
        }
        Terminator::Return(op) => {
            if matches!(f.return_ty, Ty::U0 | Ty::Error) {
                fb.ins().return_(&[]);
            } else {
                let want = clif_ty(f.return_ty, module).unwrap_or(ir::types::I32);
                let v = read_operand(fb, f, op, local_var, want);
                fb.ins().return_(&[v]);
            }
        }
        Terminator::Call {
            callee,
            args,
            dst,
            target_bb,
        } => {
            let func_ref = module.declare_func_in_func(fn_ids[callee.0 as usize], fb.func);
            let mut arg_vals = Vec::with_capacity(args.len());
            let callee_fn = &f; // dummy
            let _ = callee_fn;
            // We need the callee's signature to know the expected arg
            // types. The simplest approach: read off the FuncRef's
            // signature from the function's declared signature.
            let sig_ref = fb.func.dfg.ext_funcs[func_ref].signature;
            for (i, op) in args.iter().enumerate() {
                let want = fb.func.dfg.signatures[sig_ref].params[i].value_type;
                arg_vals.push(read_operand(fb, f, op, local_var, want));
            }
            let inst = fb.ins().call(func_ref, &arg_vals);
            // Capture the result(s) into `dst`.
            let results = fb.inst_results(inst).to_vec();
            if !results.is_empty() {
                fb.def_var(local_var[dst], results[0]);
            }
            fb.ins().jump(clif_block[target_bb], &[]);
            let _ = callee;
        }
        Terminator::Unreachable => {
            // Cranelift requires every block to have a terminator.
            // Issue a trap with a synthetic code so codegen stays sound
            // even if we accidentally reach this block at runtime.
            fb.ins().trap(ir::TrapCode::user(1).expect("trap code"));
        }
    }
}

fn lower_rvalue(
    fb: &mut FunctionBuilder<'_>,
    module: &mut ObjectModule,
    f: &MirFn,
    rv: &Rvalue,
    local_var: &FxHashMap<Local, Variable>,
) -> ir::Value {
    match rv {
        Rvalue::Use(op) => read_operand(fb, f, op, local_var, ir::types::I32),
        Rvalue::BinOp { op, lhs, rhs, ty } => {
            let clt = clif_ty(*ty, module).unwrap_or(ir::types::I32);
            let l = read_operand(fb, f, lhs, local_var, clt);
            let r = read_operand(fb, f, rhs, local_var, clt);
            lower_binop(fb, *op, *ty, l, r)
        }
        Rvalue::UnOp { op, operand, ty } => {
            let clt = clif_ty(*ty, module).unwrap_or(ir::types::I32);
            let v = read_operand(fb, f, operand, local_var, clt);
            lower_unop(fb, *op, *ty, v)
        }
    }
}

fn read_operand(
    fb: &mut FunctionBuilder<'_>,
    f: &MirFn,
    op: &Operand,
    local_var: &FxHashMap<Local, Variable>,
    want: ir::Type,
) -> ir::Value {
    match op {
        Operand::Const(c) => emit_const(fb, c, want),
        Operand::Local(l) => {
            let var = local_var[l];
            let v = fb.use_var(var);
            // For Phase 1 we trust the type checker to align operand
            // and want types; if they ever diverge (e.g. shift count
            // narrower than the value), Cranelift's verifier will trip.
            let _ = f;
            let _ = MemFlags::new(); // touch import
            v
        }
    }
}

fn emit_const(fb: &mut FunctionBuilder<'_>, c: &Const, want: ir::Type) -> ir::Value {
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
        BinOp::Eq => fb.ins().icmp(ir::condcodes::IntCC::Equal, l, r),
        BinOp::Ne => fb.ins().icmp(ir::condcodes::IntCC::NotEqual, l, r),
        BinOp::Lt => fb.ins().icmp(
            if signed {
                ir::condcodes::IntCC::SignedLessThan
            } else {
                ir::condcodes::IntCC::UnsignedLessThan
            },
            l,
            r,
        ),
        BinOp::Le => fb.ins().icmp(
            if signed {
                ir::condcodes::IntCC::SignedLessThanOrEqual
            } else {
                ir::condcodes::IntCC::UnsignedLessThanOrEqual
            },
            l,
            r,
        ),
        BinOp::Gt => fb.ins().icmp(
            if signed {
                ir::condcodes::IntCC::SignedGreaterThan
            } else {
                ir::condcodes::IntCC::UnsignedGreaterThan
            },
            l,
            r,
        ),
        BinOp::Ge => fb.ins().icmp(
            if signed {
                ir::condcodes::IntCC::SignedGreaterThanOrEqual
            } else {
                ir::condcodes::IntCC::UnsignedGreaterThanOrEqual
            },
            l,
            r,
        ),
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

    #[test]
    fn host_triple_resolves() {
        // Smoke test: the host triple should be lookup-able. This
        // verifies cranelift's ISA detection works on the dev box.
        let _isa = isa::lookup(Triple::host()).expect("host isa");
    }
}
