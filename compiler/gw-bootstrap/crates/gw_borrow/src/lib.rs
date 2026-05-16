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
//! Phase 3 increment B.4 adds a region-origin analysis: each
//! ref-typed local gets a set of "anchor" origins (either a
//! parameter, which outlives the fn return, or a let-binding,
//! which dies at fn return). Returns whose ref traces back to a
//! let-binding anchor emit `BORROW_OUTLIVES_FN` (E0401). This
//! catches the canonical dangling-borrow shape `fn dangle() ->
//! &i32 { let x: i32 = 5; return &x; }` without requiring the
//! full loan-tracking machinery of B.5. The origin sets double
//! as the seed data structure B.5's loan-tracking dataflow will
//! consume to compute "which loans are still in scope at point
//! P".
//!
//! See `docs/architecture.md` Part D.5 for the long-form design.

use gw_lex::diag::{Diagnostic, Label};
use gw_mir::{
    BlockId, Const, Local, MirBlock, MirFn, MirProgram, MirStmt, Operand, Rvalue, Terminator,
};
use gw_typeck::Ty;
use rustc_hash::{FxHashMap, FxHashSet};

/// Error codes raised by the borrow checker (E0400-series).
pub mod ec {
    use gw_lex::ErrorCode;
    /// A local is read on at least one control-flow path where it
    /// has not been initialized.
    pub const USE_OF_UNINIT_LOCAL: ErrorCode = ErrorCode(400);
    /// A returned reference traces back to a local whose storage
    /// ends at the function's return — the caller would receive a
    /// dangling pointer.
    pub const BORROW_OUTLIVES_FN: ErrorCode = ErrorCode(401);
}

/// Run the borrow-checker pipeline over `prog` and return all
/// accumulated diagnostics. An empty vec means the program is
/// borrow-clean. Currently includes B.3's init dataflow and B.4's
/// region-origin analysis.
pub fn check_program(prog: &MirProgram) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for f in &prog.functions {
        check_fn(f, &mut diags);
        check_region_origins(f, &mut diags);
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

/// Anchor for the "where does this reference's storage live"
/// origin analysis. A `Param` anchor is a parameter local — its
/// storage lives in the caller's frame and outlives the fn
/// return, so returning a ref anchored to it is safe. A
/// `LocalScope` anchor is a let-binding local — its storage
/// dies when the fn returns, so returning a ref anchored to it
/// would hand the caller a dangling pointer.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
enum Origin {
    Param(Local),
    LocalScope(Local),
}

/// Phase 3 increment B.4: region-origin analysis on MIR. For
/// each ref-typed local, compute the set of `Origin` anchors
/// its storage transitively points at. On `Return(Local(r))`
/// where `r` is ref-typed, if any anchor in `origins[r]` is a
/// `LocalScope`, the returned pointer would dangle — emit
/// `BORROW_OUTLIVES_FN` (E0401).
fn check_region_origins(f: &MirFn, diags: &mut Vec<Diagnostic>) {
    if f.blocks.is_empty() {
        return;
    }

    // Build the initial origin map by scanning every `Rvalue::Ref`
    // in the function. Each ref expression seeds its dst local's
    // origin set with one anchor that classifies the target as
    // either a parameter or a let-scope local.
    let params: FxHashSet<Local> = f.params.iter().copied().collect();
    let mut origins: FxHashMap<Local, FxHashSet<Origin>> = FxHashMap::default();
    for blk in &f.blocks {
        for stmt in &blk.statements {
            if let MirStmt::Assign {
                dst,
                value: Rvalue::Ref { target, .. },
            } = stmt
            {
                let anchor = if params.contains(target) {
                    Origin::Param(*target)
                } else {
                    Origin::LocalScope(*target)
                };
                origins.entry(*dst).or_default().insert(anchor);
            }
        }
    }

    // Fixpoint-propagate origins along ref-typed assignment chains
    // and through call-result locals. The lattice is
    // `(FxHashMap<Local, FxHashSet<Origin>>, ⊆)`; transfer is
    // monotone (only ever inserts), so a single re-iteration loop
    // converges in O(stmts × distinct-locals) time.
    let mut changed = true;
    while changed {
        changed = false;
        for blk in &f.blocks {
            for stmt in &blk.statements {
                // `dst = Use(src)` propagates src's origins if src
                // is ref-typed. Ignore non-ref locals — their
                // origin sets are uninteresting.
                if let MirStmt::Assign {
                    dst,
                    value: Rvalue::Use(Operand::Local(src)),
                } = stmt
                {
                    if is_ref(f, *src) && propagate(&mut origins, *src, *dst) {
                        changed = true;
                    }
                }
            }
            // A call that returns a `&T` produces a ref-typed dst.
            // Per Phase-1 elision (no explicit lifetime
            // annotations) the conservative rule is: dst's origin
            // set is the union of every ref-typed argument's
            // origin set. This matches Rust's elision when the
            // return lifetime equals any one of the input
            // lifetimes — for "function-local" regions it's
            // sufficient because the only thing we ask of `dst`
            // is whether it traces back to a caller-side anchor.
            if let Terminator::Call { args, dst, .. } = &blk.terminator {
                if is_ref(f, *dst) {
                    let mut union: FxHashSet<Origin> = FxHashSet::default();
                    for a in args {
                        if let Operand::Local(a_local) = a {
                            if is_ref(f, *a_local) {
                                if let Some(s) = origins.get(a_local) {
                                    union.extend(s.iter().copied());
                                }
                            }
                        }
                    }
                    if !union.is_empty() {
                        let entry = origins.entry(*dst).or_default();
                        for o in union {
                            if entry.insert(o) {
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
    }

    // Diagnostic pass: every `Return(Local(r))` where `r` is
    // ref-typed and `origins[r]` contains at least one
    // `LocalScope` anchor is a dangling-borrow return. Emit one
    // diagnostic per offending fn-return — dedupe via a flag so
    // multiple-return-block shapes don't double-fire on the same
    // origin chain.
    let mut reported = false;
    for blk in &f.blocks {
        if reported {
            break;
        }
        let Terminator::Return(Operand::Local(r)) = &blk.terminator else {
            continue;
        };
        if !is_ref(f, *r) {
            continue;
        }
        let Some(anchors) = origins.get(r) else {
            continue;
        };
        let dangling_anchor = anchors.iter().find(|o| matches!(o, Origin::LocalScope(_)));
        if let Some(Origin::LocalScope(local)) = dangling_anchor {
            diags.push(make_dangling_diag(f, *local));
            reported = true;
        }
    }
}

fn propagate(origins: &mut FxHashMap<Local, FxHashSet<Origin>>, src: Local, dst: Local) -> bool {
    let src_set = match origins.get(&src) {
        Some(s) if !s.is_empty() => s.clone(),
        _ => return false,
    };
    let entry = origins.entry(dst).or_default();
    let mut changed = false;
    for o in src_set {
        if entry.insert(o) {
            changed = true;
        }
    }
    changed
}

fn is_ref(f: &MirFn, l: Local) -> bool {
    matches!(f.locals[l.0 as usize].ty, Ty::Ref { .. })
}

fn make_dangling_diag(f: &MirFn, local: Local) -> Diagnostic {
    let span = f.locals[local.0 as usize].span;
    let mut d = Diagnostic::error(
        ec::BORROW_OUTLIVES_FN,
        Label::new(span, "borrowed local declared here"),
        "returned reference outlives the function — borrow points at a local whose storage ends at this fn's return",
    );
    d.notes.push(
        "Phase 3 B.4 enforces region-validity on function returns: \
         a returned `&T` must trace back to a parameter (caller-owned) \
         rather than a let-binding (callee-scope). Either return by \
         value, or rewrite the API so the caller supplies the storage \
         (`fn out(out: &mut T) { ... }`)."
            .into(),
    );
    d
}

/// Re-export the borrow-checker error-code namespace so callers
/// (the driver, tests) can name codes without depending directly
/// on `gw_lex::ErrorCode` numerics.
pub use ec::{BORROW_OUTLIVES_FN, USE_OF_UNINIT_LOCAL};
