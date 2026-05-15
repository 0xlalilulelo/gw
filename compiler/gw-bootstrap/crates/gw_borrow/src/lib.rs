//! GW borrow / lifetime checker (Phase 3+).
//!
//! Phase 3 increment B.3 installs the first dataflow pass: a
//! `MaybeInitialized` lattice on MIR locals. Reads of a local that
//! is not definitely-initialized at the read site emit
//! `USE_OF_UNINIT_LOCAL` (E0400). Move tracking proper (move-out
//! transitions on aggregates) rides B.4+; today Phase 1 has no
//! Move-typed surface, so the only thing the framework catches is
//! genuinely-uninit reads from `let x: T;` (no initialiser) and
//! conditional inits that don't cover every path.
//!
//! See `docs/architecture.md` Part D.5 for the long-form design.

use gw_lex::diag::{Diagnostic, Label};
use gw_mir::{
    BlockId, Const, Local, MirBlock, MirFn, MirProgram, MirStmt, Operand, Rvalue, Terminator,
};
use rustc_hash::FxHashSet;

/// Error codes raised by the borrow checker (E0400-series).
pub mod ec {
    use gw_lex::ErrorCode;
    /// A local is read on at least one control-flow path where it
    /// has not been initialized.
    pub const USE_OF_UNINIT_LOCAL: ErrorCode = ErrorCode(400);
}

/// Run B.3's move-tracking dataflow over `prog` and return the
/// accumulated diagnostics. An empty vec means the program is
/// initialization-clean.
pub fn check_program(prog: &MirProgram) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for f in &prog.functions {
        check_fn(f, &mut diags);
    }
    diags
}

/// Per-function init dataflow. The lattice element at every program
/// point is the set of locals that are *definitely* initialized at
/// that point. Join at block-merge is set intersection — a local is
/// initialized on entry to a join block only if it is initialized on
/// every incoming edge.
fn check_fn(f: &MirFn, diags: &mut Vec<Diagnostic>) {
    if f.blocks.is_empty() {
        return;
    }

    // Build predecessor / successor adjacency from terminators.
    let n = f.blocks.len();
    let mut succs: Vec<Vec<BlockId>> = vec![Vec::new(); n];
    let mut preds: Vec<Vec<BlockId>> = vec![Vec::new(); n];
    for (i, blk) in f.blocks.iter().enumerate() {
        let from = BlockId(i as u32);
        for to in terminator_successors(&blk.terminator) {
            succs[i].push(to);
            preds[to.0 as usize].push(from);
        }
    }

    // Initial state: entry block's in-set = params (parameters are
    // initialized on fn entry). All other blocks start with `None` —
    // they haven't been visited yet, so their in-set is unknown.
    // Using `Option` lets the join operator distinguish "no
    // predecessors processed" from "empty intersection".
    let mut block_in: Vec<Option<FxHashSet<Local>>> = vec![None; n];
    let mut block_out: Vec<Option<FxHashSet<Local>>> = vec![None; n];
    let entry_in: FxHashSet<Local> = f.params.iter().copied().collect();
    block_in[0] = Some(entry_in.clone());
    block_out[0] = Some(transfer_block(&f.blocks[0], &entry_in));

    // Worklist iteration. Push successors of any block whose out-set
    // changes. Terminates because the lattice (P(Locals), ⊆) has
    // bounded height and the transfer / join operators are monotone
    // wrt set growth (initialization only adds; never removes).
    let mut work: Vec<BlockId> = succs[0].clone();
    while let Some(b) = work.pop() {
        let i = b.0 as usize;

        // Join: intersect over all predecessors that have a computed
        // out-set. A predecessor with `None` out hasn't been reached
        // by the worklist yet — ignore it, it will re-enqueue us
        // when it gets computed.
        let mut new_in: Option<FxHashSet<Local>> = None;
        for &p in &preds[i] {
            let Some(po) = &block_out[p.0 as usize] else {
                continue;
            };
            new_in = Some(match new_in {
                None => po.clone(),
                Some(cur) => cur.intersection(po).copied().collect(),
            });
        }
        let new_in = new_in.unwrap_or_default();

        // Skip if in-set unchanged.
        if block_in[i].as_ref().is_some_and(|cur| cur == &new_in) {
            continue;
        }
        block_in[i] = Some(new_in.clone());
        let new_out = transfer_block(&f.blocks[i], &new_in);
        let out_changed = block_out[i].as_ref().is_none_or(|cur| cur != &new_out);
        block_out[i] = Some(new_out);
        if out_changed {
            for &s in &succs[i] {
                work.push(s);
            }
        }
    }

    // Diagnostic pass: walk each reachable block with its converged
    // in-state and emit one diag per local with a possibly-uninit
    // read. Dedupe by Local — multiple reads of the same uninit
    // local produce a single E0400.
    let mut reported: FxHashSet<Local> = FxHashSet::default();
    for (i, blk) in f.blocks.iter().enumerate() {
        let Some(in_set) = block_in[i].clone() else {
            continue; // unreachable block
        };
        let mut cur = in_set;
        for stmt in &blk.statements {
            for r in stmt_reads(stmt) {
                if !cur.contains(&r) && !reported.contains(&r) {
                    diags.push(make_uninit_diag(f, r));
                    reported.insert(r);
                }
            }
            if let Some(w) = stmt_write(stmt) {
                cur.insert(w);
            }
        }
        for r in terminator_reads(&blk.terminator) {
            if !cur.contains(&r) && !reported.contains(&r) {
                diags.push(make_uninit_diag(f, r));
                reported.insert(r);
            }
        }
        if let Some(w) = terminator_write(&blk.terminator) {
            cur.insert(w);
        }
    }
}

/// Apply a block's transfer function: start from `in_set`, walk
/// statements + terminator, accumulate writes.
fn transfer_block(blk: &MirBlock, in_set: &FxHashSet<Local>) -> FxHashSet<Local> {
    let mut cur = in_set.clone();
    for stmt in &blk.statements {
        if let Some(w) = stmt_write(stmt) {
            cur.insert(w);
        }
    }
    if let Some(w) = terminator_write(&blk.terminator) {
        cur.insert(w);
    }
    cur
}

/// Locals that a statement reads — these must be initialized at the
/// statement's program point.
fn stmt_reads(s: &MirStmt) -> Vec<Local> {
    let mut r = Vec::new();
    match s {
        MirStmt::Assign { value, .. } => collect_rvalue_reads(value, &mut r),
        MirStmt::AssignField { value, .. } => {
            // `x.f = v` is how struct-literal lowering builds an
            // aggregate field-by-field (`lower_struct_lit` emits one
            // `AssignField` per field on a freshly-allocated dst
            // that has no prior `Assign`). B.3 therefore treats
            // `AssignField` as a *write* of the dst — it transfers
            // it from uninit → init — and does not require the dst
            // to already be init. Partial-init tracking (only some
            // fields written before a whole-aggregate read) rides
            // a future sub-bundle alongside field-level move
            // tracking.
            collect_rvalue_reads(value, &mut r);
        }
        MirStmt::StoreThroughRef { ptr, value, .. } => {
            push_operand_local(ptr, &mut r);
            push_operand_local(value, &mut r);
        }
    }
    r
}

/// Local that a statement writes (transfers from uninit → init).
fn stmt_write(s: &MirStmt) -> Option<Local> {
    match s {
        MirStmt::Assign { dst, .. } => Some(*dst),
        // `AssignField` is how struct-literal lowering builds the
        // aggregate; treat it as init-transferring for the dst.
        // See `stmt_reads` for the partial-init caveat.
        MirStmt::AssignField { dst, .. } => Some(*dst),
        // `*r = v` writes through a pointer — it doesn't init any
        // local in the current frame.
        MirStmt::StoreThroughRef { .. } => None,
    }
}

fn collect_rvalue_reads(rv: &Rvalue, out: &mut Vec<Local>) {
    match rv {
        Rvalue::Use(op) => push_operand_local(op, out),
        Rvalue::BinOp { lhs, rhs, .. } => {
            push_operand_local(lhs, out);
            push_operand_local(rhs, out);
        }
        Rvalue::UnOp { operand, .. } => push_operand_local(operand, out),
        Rvalue::Field { base, .. } => out.push(*base),
        Rvalue::Cast { operand, .. } => push_operand_local(operand, out),
        Rvalue::Ref { target, .. } => out.push(*target),
        Rvalue::Deref { ptr, .. } => push_operand_local(ptr, out),
    }
}

fn push_operand_local(op: &Operand, out: &mut Vec<Local>) {
    match op {
        Operand::Local(l) => out.push(*l),
        Operand::Const(Const::Error) | Operand::Const(_) => {}
    }
}

fn terminator_reads(t: &Terminator) -> Vec<Local> {
    let mut r = Vec::new();
    match t {
        Terminator::Goto(_) | Terminator::Unreachable => {}
        Terminator::Branch { cond, .. } => push_operand_local(cond, &mut r),
        Terminator::Return(op) => push_operand_local(op, &mut r),
        Terminator::Call { args, .. } => {
            for a in args {
                push_operand_local(a, &mut r);
            }
        }
    }
    r
}

fn terminator_write(t: &Terminator) -> Option<Local> {
    match t {
        Terminator::Call { dst, .. } => Some(*dst),
        _ => None,
    }
}

fn terminator_successors(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Goto(b) => vec![*b],
        Terminator::Branch {
            then_bb, else_bb, ..
        } => vec![*then_bb, *else_bb],
        Terminator::Call { target_bb, .. } => vec![*target_bb],
        Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
    }
}

fn make_uninit_diag(f: &MirFn, local: Local) -> Diagnostic {
    let span = f.locals[local.0 as usize].span;
    let mut d = Diagnostic::error(
        ec::USE_OF_UNINIT_LOCAL,
        Label::new(span, "declared here"),
        "use of possibly-uninitialized local",
    );
    d.notes.push(
        "all paths into the use site must initialize the local; \
         either move the initializer earlier, or assign in every \
         branch of the preceding control flow."
            .into(),
    );
    d
}

/// Re-export the borrow-checker error-code namespace so callers
/// (the driver, tests) can name codes without depending directly
/// on `gw_lex::ErrorCode` numerics.
pub use ec::USE_OF_UNINIT_LOCAL;
