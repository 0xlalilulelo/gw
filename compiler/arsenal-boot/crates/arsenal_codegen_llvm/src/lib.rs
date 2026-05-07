//! GW LLVM backend (Phase 13).
//!
//! Mirrors the public contract of [`arsenal_codegen_fast::compile_program`]:
//! `MirProgram → object bytes`. The driver picks between the two backends
//! at `arsenal build --backend=fast|llvm`.
//!
//! B.1 (this commit) is the tracer bullet: only enough surface to compile
//! `fn main() -> i32 { return 0; }`. Every other construct returns
//! [`CodegenError::Unsupported`]. B.2–B.5 widen the supported subset
//! incrementally; the structure here is intentionally shaped so each
//! later increment fills in `match` arms rather than reshaping the
//! pipeline.

use arsenal_mir::{Const, MirFn, MirProgram, Operand, Terminator};
use arsenal_typeck::{IntTy, Ty};
use inkwell::basic_block::BasicBlock;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetTriple,
};
use inkwell::types::{BasicMetadataTypeEnum, FunctionType};
use inkwell::values::FunctionValue;
use inkwell::OptimizationLevel;
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
    // (B.1 has no calls, but the structure stays — B.2 needs this.)
    let mut fn_values: Vec<FunctionValue<'_>> = Vec::with_capacity(prog.functions.len());
    for f in &prog.functions {
        let fn_ty = make_fn_type(&context, f)?;
        let linkage = if f.is_extern {
            Some(Linkage::External)
        } else {
            // `Linkage::External` here means "exported, defined in this
            // module" — matches Cranelift's `Linkage::Export`. LLVM
            // overloads the same name for both directions; the
            // import/export distinction is implicit from whether the
            // function has a body.
            Some(Linkage::External)
        };
        let function = module.add_function(&f.name, fn_ty, linkage);
        fn_values.push(function);
    }

    // Pass 2: define non-extern bodies.
    for (f, function) in prog.functions.iter().zip(fn_values.iter().copied()) {
        if f.is_extern {
            continue;
        }
        define_fn(&context, &module, f, function)?;
    }

    // Verify the module before emitting. Catches malformed IR with a
    // useful error rather than letting `write_to_memory_buffer` produce
    // mystery output. Disabled in release builds is tempting, but
    // bootstrap-stage verification cost is negligible.
    module
        .verify()
        .map_err(|e| CodegenError::Builder(e.to_string()))?;

    let buffer = machine
        .write_to_memory_buffer(&module, FileType::Object)
        .map_err(|e| CodegenError::Emit(e.to_string()))?;
    Ok(buffer.as_slice().to_vec())
}

fn define_fn(
    context: &Context,
    _module: &Module<'_>,
    f: &MirFn,
    function: FunctionValue<'_>,
) -> Result<(), CodegenError> {
    if !f.params.is_empty() {
        return Err(CodegenError::Unsupported(format!(
            "fn `{}` has parameters; B.1 tracer-bullet supports zero-arg fns only",
            f.name
        )));
    }

    // Pre-create one LLVM basic block per MIR block so terminators can
    // reference forward (Goto / Branch land in B.2; pre-creation keeps
    // that change to an arm rather than a restructure).
    let bbs: Vec<BasicBlock<'_>> = (0..f.blocks.len())
        .map(|i| context.append_basic_block(function, &format!("bb{i}")))
        .collect();

    let builder = context.create_builder();
    for (i, mir_block) in f.blocks.iter().enumerate() {
        builder.position_at_end(bbs[i]);

        if !mir_block.statements.is_empty() {
            return Err(CodegenError::Unsupported(format!(
                "fn `{}` block {} has {} MIR statement(s); B.1 supports empty bodies only",
                f.name,
                i,
                mir_block.statements.len()
            )));
        }

        match &mir_block.terminator {
            Terminator::Return(Operand::Const(Const::Int { value, ty })) => {
                let int_ty = llvm_int_type(context, *ty);
                // `as u64` truncates the i128 const to 64 bits; for the
                // I8..I64 widths the value is exact. `sign_extend` is
                // false because inkwell's `const_int` interprets the
                // bits literally — the i128 already carries the signed
                // value.
                let value_const = int_ty.const_int(*value as u64, false);
                builder
                    .build_return(Some(&value_const))
                    .map_err(|e| CodegenError::Builder(e.to_string()))?;
            }
            Terminator::Unreachable => {
                builder
                    .build_unreachable()
                    .map_err(|e| CodegenError::Builder(e.to_string()))?;
            }
            other => {
                return Err(CodegenError::Unsupported(format!(
                    "fn `{}` block {} terminator {:?}; B.1 supports \
                     `Return(Const::Int)` and `Unreachable` only",
                    f.name, i, other
                )));
            }
        }
    }
    Ok(())
}

fn make_fn_type<'ctx>(
    context: &'ctx Context,
    f: &MirFn,
) -> Result<FunctionType<'ctx>, CodegenError> {
    if !f.params.is_empty() {
        return Err(CodegenError::Unsupported(format!(
            "fn `{}` has parameters; B.1 supports zero-arg fns only",
            f.name
        )));
    }
    let params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
    Ok(match f.return_ty {
        Ty::U0 => context.void_type().fn_type(&params, false),
        Ty::Int(int_ty) => llvm_int_type(context, int_ty).fn_type(&params, false),
        ref other => {
            return Err(CodegenError::Unsupported(format!(
                "fn `{}` return type {:?}; B.1 supports `u0` and integer returns",
                f.name, other
            )));
        }
    })
}

fn llvm_int_type<'ctx>(context: &'ctx Context, ty: IntTy) -> inkwell::types::IntType<'ctx> {
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
