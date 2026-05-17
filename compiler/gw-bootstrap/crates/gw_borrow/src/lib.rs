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
//! as the seed data structure B.5's loan-tracking dataflow
//! consumes to compute "which loans are still in scope at point
//! P".
//!
//! Phase 3 increment B.5 installs the borrow checker proper:
//! per-fn loan tracking + the aliasing rule on mutating
//! accesses. Each `Rvalue::Ref` site is a *loan*
//! `(place, mutable, span)`; the dataflow lattice is
//! `(2^Loans, ⊆)`; the gen-only-no-kill transfer matches the
//! architecture's "function-local, scope-bounded" simplification
//! (D.5 step 2 — loans live to end-of-fn because Phase 1 has no
//! sub-fn binding scopes that drop refs early). At each
//! mutating access, walk live loans and check the aliasing rule
//! (D.5 step 3): a new mutable borrow of place `P` conflicts
//! with any existing loan on `P`; a new shared borrow conflicts
//! with any existing *mutable* loan on `P`; a direct write to
//! local `L` conflicts with any loan on `L`; a write *through*
//! `*r` checks loans on `origins[r]`'s anchors (excluding the
//! loans `r` itself holds — those *are* the legitimate use).
//! Shared-read aliasing checks (read-while-mut) are deferred to
//! B.5b — they require a backward liveness pass on ref locals
//! to compute loan-death = last-use of any holder; without that,
//! end-of-fn loan death would reject existing positive fixtures
//! like `let r = &mut x; *r = 10; return x;` (242) which Rust's
//! NLL accepts. The mutating-access checks alone catch the three
//! canonical conflict shapes (two `&mut`, shared + `&mut`,
//! direct write to borrowed local) without false-positives on
//! the existing 248-fixture phase1 corpus.
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
    /// Two borrows of the same place conflict per Phase-1's
    /// function-scoped aliasing rules: a mutable borrow with any
    /// other borrow of the same place; a shared borrow with a
    /// mutable borrow; or a direct write to a borrowed local.
    pub const CONFLICTING_BORROW: ErrorCode = ErrorCode(402);
}

/// Run the borrow-checker pipeline over `prog` and return all
/// accumulated diagnostics. An empty vec means the program is
/// borrow-clean. Currently includes B.3's init dataflow, B.4's
/// region-origin analysis, and B.5's loan-tracking aliasing
/// check (which consumes B.4's origins for through-pointer
/// access resolution).
pub fn check_program(prog: &MirProgram) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for f in &prog.functions {
        check_fn(f, &mut diags);
        let origins = compute_origins(f);
        check_region_origins(f, &origins, &mut diags);
        check_loans(f, &origins, &mut diags);
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

/// Phase 3 increment B.4: build the per-fn region-origin map.
/// For each ref-typed local, compute the set of `Origin`
/// anchors its storage transitively points at. The map is then
/// consumed by `check_region_origins` (emits E0401 on dangling
/// returns) and by `check_loans` (resolves through-pointer
/// accesses to their underlying places for the aliasing check).
/// Algorithm: seed by scanning every `Rvalue::Ref`, then
/// fixpoint-propagate along ref-typed `Assign { dst, Use(src) }`
/// chains and through `Terminator::Call { args, dst }` where
/// `dst` is ref-typed (Phase-1 elision rule: union over ref-
/// typed args). The lattice is monotone-grow-only so the loop
/// converges in O(stmts × distinct-locals).
fn compute_origins(f: &MirFn) -> FxHashMap<Local, FxHashSet<Origin>> {
    let mut origins: FxHashMap<Local, FxHashSet<Origin>> = FxHashMap::default();
    if f.blocks.is_empty() {
        return origins;
    }
    let params: FxHashSet<Local> = f.params.iter().copied().collect();
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
    let mut changed = true;
    while changed {
        changed = false;
        for blk in &f.blocks {
            for stmt in &blk.statements {
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
    origins
}

/// Phase 3 increment B.4: emit `BORROW_OUTLIVES_FN` (E0401) for
/// any `Return(Local(r))` where `r` is ref-typed and
/// `origins[r]` contains at least one `LocalScope` anchor (the
/// returned pointer would dangle). One diag per fn — multiple
/// return blocks tracing the same origin chain fire once.
fn check_region_origins(
    f: &MirFn,
    origins: &FxHashMap<Local, FxHashSet<Origin>>,
    diags: &mut Vec<Diagnostic>,
) {
    if f.blocks.is_empty() {
        return;
    }
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

/// Read the mutability bit of a ref-typed local. Returns `None`
/// if the local isn't a `Ty::Ref { .. }`; callers should only
/// invoke this on locals known to be ref-typed (e.g., the `dst`
/// of an `Assign { value: Rvalue::Ref { .. } }`).
fn ref_mutability(f: &MirFn, l: Local) -> Option<bool> {
    match f.locals[l.0 as usize].ty {
        Ty::Ref { mutable, .. } => Some(mutable),
        _ => None,
    }
}

/// A loan record: one entry per `Rvalue::Ref` site in the fn.
/// `place` is the borrowed local (the `target` of the ref);
/// `mutable` is read from the *dst* local's `Ty::Ref { mutable }`
/// (the language doesn't track mutability on the rvalue itself —
/// it lives on the ref's type). The diagnostic primary span is
/// always the conflicted-over *place*'s `LocalDecl.span`
/// (matching B.3's E0400 and B.4's E0401 convention), so the
/// loan itself doesn't need to carry a span yet — when B.5b
/// adds a secondary "earlier borrow here" label it will be
/// reintroduced.
#[derive(Clone, Debug)]
struct Loan {
    place: Local,
    mutable: bool,
}

/// Index into the per-fn `loans: Vec<Loan>` table.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
struct LoanId(u32);

/// Phase 3 increment B.5: loan tracking + aliasing-rule check
/// on mutating accesses. Architecture spec: D.5 steps 2–3.
///
/// Builds a per-fn loan table from every `Rvalue::Ref` site,
/// computes the per-local "holds" map (which loans each ref-
/// typed local can carry — same fixpoint shape as B.4's
/// `compute_origins`), runs forward dataflow on the live-loan
/// lattice (union join, gen-only-no-kill — matching the
/// architecture's function-local region simplification:
/// loans live to end-of-fn because Phase 1 has no sub-fn
/// binding scopes that drop refs early), then walks each
/// block's stmts + terminator with the running live-loan set
/// and emits `CONFLICTING_BORROW` (E0402) on the four mutating
/// access shapes:
///
/// 1. **Ref gen site** (`Assign { dst, Rvalue::Ref { target } }`):
///    a new mutable borrow conflicts with any existing loan on
///    `target`; a new shared borrow conflicts with any
///    existing *mutable* loan on `target`.
/// 2. **Direct write of a local** (non-Ref `Assign.dst`,
///    `AssignField.dst`, `Call.dst` from the terminator): mut
///    access; conflicts with any live loan on the local.
/// 3. **`StoreThroughRef { ptr, .. }`**: through-pointer mut
///    access on the place(s) `origins[ptr]` anchors at; checks
///    loans on each anchor place *excluding* the loans `ptr`
///    itself holds (those *are* the legitimate use).
///
/// Shared-read aliasing checks (read-while-mut) are NOT
/// performed here — they need a backward liveness pass on ref
/// locals (B.5b) to compute loan-death = last-use-of-holder so
/// existing positive fixtures like `let r = &mut x; *r = 10;
/// return x;` (242) keep passing. Under the end-of-fn loan-
/// death rule a naive shared-read check would reject those.
///
/// Diagnostics are deduped by `place: Local` — one E0402 per
/// borrowed local per fn — so a single program with three
/// kinds of conflicts on `x` produces one diagnostic, matching
/// B.3's per-local dedupe.
fn check_loans(
    f: &MirFn,
    origins: &FxHashMap<Local, FxHashSet<Origin>>,
    diags: &mut Vec<Diagnostic>,
) {
    if f.blocks.is_empty() {
        return;
    }

    // 1. Build the loan table by scanning every `Rvalue::Ref`
    //    site. Each site is assigned a fresh `LoanId`; the
    //    map `loan_at_stmt` lets the dataflow + diag passes
    //    re-locate the loan from (block_idx, stmt_idx) without
    //    rescanning the rvalue.
    let mut loans: Vec<Loan> = Vec::new();
    let mut loan_at_stmt: FxHashMap<(usize, usize), LoanId> = FxHashMap::default();
    for (bi, blk) in f.blocks.iter().enumerate() {
        for (si, stmt) in blk.statements.iter().enumerate() {
            let MirStmt::Assign {
                dst,
                value: Rvalue::Ref { target, .. },
            } = stmt
            else {
                continue;
            };
            // Mutability lives on the dst's `Ty::Ref { mutable }`.
            // Skip if the dst isn't ref-typed (shouldn't happen in
            // well-typed MIR, but a `Ty::Error` arm would slip
            // through if typeck previously errored — be defensive).
            let Some(mutable) = ref_mutability(f, *dst) else {
                continue;
            };
            let id = LoanId(loans.len() as u32);
            loans.push(Loan {
                place: *target,
                mutable,
            });
            loan_at_stmt.insert((bi, si), id);
        }
    }
    if loans.is_empty() {
        return; // no `&` in this fn — no loans, no aliasing checks needed.
    }

    // 2. Compute the per-local "holds" map: which loans each
    //    local can carry. Same fixpoint shape as `compute_origins`
    //    but tracking `LoanId` instead of `Origin` anchors.
    //    Used at through-pointer access sites to exclude the
    //    loan whose pointer is being legitimately used.
    let mut holds: FxHashMap<Local, FxHashSet<LoanId>> = FxHashMap::default();
    // Seed: each ref gen-site dst holds the new loan.
    for ((bi, si), id) in &loan_at_stmt {
        if let MirStmt::Assign { dst, .. } = &f.blocks[*bi].statements[*si] {
            holds.entry(*dst).or_default().insert(*id);
        }
    }
    // Propagate.
    let mut changed = true;
    while changed {
        changed = false;
        for blk in &f.blocks {
            for stmt in &blk.statements {
                if let MirStmt::Assign {
                    dst,
                    value: Rvalue::Use(Operand::Local(src)),
                } = stmt
                {
                    if is_ref(f, *src) {
                        if let Some(src_set) = holds.get(src).cloned() {
                            let entry = holds.entry(*dst).or_default();
                            for id in src_set {
                                if entry.insert(id) {
                                    changed = true;
                                }
                            }
                        }
                    }
                }
            }
            if let Terminator::Call { args, dst, .. } = &blk.terminator {
                if is_ref(f, *dst) {
                    let mut union: FxHashSet<LoanId> = FxHashSet::default();
                    for a in args {
                        if let Operand::Local(al) = a {
                            if is_ref(f, *al) {
                                if let Some(s) = holds.get(al) {
                                    union.extend(s.iter().copied());
                                }
                            }
                        }
                    }
                    if !union.is_empty() {
                        let entry = holds.entry(*dst).or_default();
                        for id in union {
                            if entry.insert(id) {
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Forward dataflow on the live-loan lattice.
    //    `block_in[B] = ⋃ block_out[pred]`,
    //    `block_out[B] = block_in[B] ∪ block_gen[B]`.
    //    Gen-only (no kill — matches end-of-fn region death).
    //    Iterate to fixpoint; monotone-grow-only ⇒ termination
    //    in O(blocks × loans).
    let n = f.blocks.len();
    let mut block_in: Vec<FxHashSet<LoanId>> = vec![FxHashSet::default(); n];
    let mut block_out: Vec<FxHashSet<LoanId>> = vec![FxHashSet::default(); n];
    let mut block_gen: Vec<FxHashSet<LoanId>> = vec![FxHashSet::default(); n];
    for ((bi, _), id) in &loan_at_stmt {
        block_gen[*bi].insert(*id);
    }
    let mut preds: Vec<Vec<BlockId>> = vec![Vec::new(); n];
    for (i, blk) in f.blocks.iter().enumerate() {
        let from = BlockId(i as u32);
        for to in terminator_successors(&blk.terminator) {
            preds[to.0 as usize].push(from);
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for i in 0..n {
            let mut new_in: FxHashSet<LoanId> = FxHashSet::default();
            for &p in &preds[i] {
                new_in.extend(block_out[p.0 as usize].iter().copied());
            }
            if new_in != block_in[i] {
                block_in[i] = new_in.clone();
                changed = true;
            }
            let mut new_out = block_in[i].clone();
            new_out.extend(block_gen[i].iter().copied());
            if new_out != block_out[i] {
                block_out[i] = new_out;
                changed = true;
            }
        }
    }

    // 4. Diagnostic pass: walk each block with its `block_in` as
    //    a running live-loan set; check each mutating access; gen
    //    new loans as we encounter `Rvalue::Ref` sites; dedupe by
    //    `place: Local` so one fn with three conflicts on the
    //    same `x` produces one E0402.
    let mut reported: FxHashSet<Local> = FxHashSet::default();
    for (bi, blk) in f.blocks.iter().enumerate() {
        let mut live: FxHashSet<LoanId> = block_in[bi].clone();
        for (si, stmt) in blk.statements.iter().enumerate() {
            match stmt {
                MirStmt::Assign {
                    dst: _,
                    value: Rvalue::Ref { target, .. },
                } => {
                    // (1) Ref gen-site conflict check.
                    if let Some(new_id) = loan_at_stmt.get(&(bi, si)) {
                        let new_mut = loans[new_id.0 as usize].mutable;
                        if !reported.contains(target) {
                            for &lid in &live {
                                let l = &loans[lid.0 as usize];
                                if l.place == *target && (new_mut || l.mutable) {
                                    diags.push(make_conflicting_borrow_diag(f, *target));
                                    reported.insert(*target);
                                    break;
                                }
                            }
                        }
                        live.insert(*new_id);
                    }
                }
                MirStmt::Assign { dst, value: _ } => {
                    // (2) Direct write of dst.
                    check_direct_write(f, &loans, *dst, &live, &mut reported, diags);
                }
                MirStmt::AssignField { dst, .. } => {
                    // (2) Direct write of dst (field-by-field
                    // aggregate build still writes the whole
                    // local from the borrow checker's POV).
                    check_direct_write(f, &loans, *dst, &live, &mut reported, diags);
                }
                MirStmt::StoreThroughRef { ptr, .. } => {
                    // (3) Through-pointer mutable access.
                    let Operand::Local(ptr_local) = ptr else {
                        continue;
                    };
                    let held = holds.get(ptr_local).cloned().unwrap_or_default();
                    let Some(anchors) = origins.get(ptr_local) else {
                        continue;
                    };
                    for o in anchors {
                        let p = match o {
                            Origin::Param(l) | Origin::LocalScope(l) => *l,
                        };
                        if reported.contains(&p) {
                            continue;
                        }
                        for &lid in &live {
                            if held.contains(&lid) {
                                continue;
                            }
                            let l = &loans[lid.0 as usize];
                            if l.place == p {
                                diags.push(make_conflicting_borrow_diag(f, p));
                                reported.insert(p);
                                break;
                            }
                        }
                    }
                }
            }
        }
        // Terminator: `Call.dst` is a direct write of `dst`.
        if let Terminator::Call { dst, .. } = &blk.terminator {
            check_direct_write(f, &loans, *dst, &live, &mut reported, diags);
        }
    }
}

/// Helper for the direct-write arm of B.5: check whether any
/// live loan covers `dst` (= has `place == dst`); if so, emit
/// one E0402 per `dst` (deduped via `reported`).
fn check_direct_write(
    f: &MirFn,
    loans: &[Loan],
    dst: Local,
    live: &FxHashSet<LoanId>,
    reported: &mut FxHashSet<Local>,
    diags: &mut Vec<Diagnostic>,
) {
    if reported.contains(&dst) {
        return;
    }
    for &lid in live {
        let l = &loans[lid.0 as usize];
        if l.place == dst {
            diags.push(make_conflicting_borrow_diag(f, dst));
            reported.insert(dst);
            break;
        }
    }
}

fn make_conflicting_borrow_diag(f: &MirFn, place: Local) -> Diagnostic {
    let span = f.locals[place.0 as usize].span;
    let mut d = Diagnostic::error(
        ec::CONFLICTING_BORROW,
        Label::new(span, "borrowed local declared here"),
        "conflicting borrow",
    );
    d.notes.push(
        "Phase 3 B.5 enforces fn-scoped aliasing rules: a mutable \
         borrow conflicts with any other borrow of the same place; \
         a shared borrow conflicts with a mutable borrow; direct \
         writes to a local are disallowed while any borrow on it is \
         live. Either re-order so the conflicting borrows don't \
         overlap, or rewrite to use distinct locals."
            .into(),
    );
    d
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
pub use ec::{BORROW_OUTLIVES_FN, CONFLICTING_BORROW, USE_OF_UNINIT_LOCAL};
