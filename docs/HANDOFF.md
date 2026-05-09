# GW Bootstrap — Session Handoff

This document is the entry point for the next development session. Read it first.

> **Last updated:** after Phase-2 increment O.1 (`?T` optional type
> with `nil`, `T → ?T` coercion, and the `??` coalesce operator —
> first Phase-2 sub-bundle that introduced value-level novelty).
> **Repo root:** `/Users/silmaril/Documents/GitHub/gw`
> **Workspace test count:** 185 unit + integration tests, all green.
> **Corpus size:** 61 Phase-0 lex+parse snapshots + 239 Phase-1 / Phase-2 run-tests.
> **Phase 1 exit gate met** (200-program target hit at 12h; the post-exit
> follow-up A.1–A.4 added 26 more, for 226 total).
> **Phase 13 (LLVM backend) complete** — `arsenal build --backend=llvm`
> compiles and runs every program in the corpus, matching Cranelift exit
> codes and stdout bit-exactly. Both backends ship in the same workspace;
> `--backend=fast` (Cranelift) remains the default.
> **Phase 2 entry in progress** — c-strings sub-bundle (C.1 + C.2)
> closed first; the match sub-bundle (M.1 + M.2 + M.3) closed
> next; the `?T` tracer (O.1) closed third. `?i32` / `?bool` types
> as a 2-field `{tag: u8, payload: T}` aggregate, `nil` adopts the
> Optional, `T → ?T` coerces at let-init / return / call-arg, and
> `??` reads the tag and lazily evaluates the default on the nil
> branch. O.1 is the first Phase-2 sub-bundle that introduced
> value-level novelty (tag bytes), and the dual-backend test
> caught one bug at first run — exactly the 12/A.x prediction.
> Cleanup #1 (default `-> u0` on missing return type) shipped
> alongside C.1.

---

## TL;DR

The GW bootstrap compiler at `compiler/arsenal-boot/` is a Rust implementation
of an end-to-end pipeline that compiles a meaningful subset of GW into native
binaries:

```
fn fib(n: i32) -> i32 {
    if n < 2 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main() -> i32 { return fib(8); }
```

Compiles (via `arsenal build`) → runs natively → exits 21. Real programs.
Iterative factorial, classes with field mutation, while loops with `break`,
`for x in 0..n`, `extern fn putchar` calls into libc, top-level statements
without `fn main`, `[]u8` string slices, the canonical Phase-1 hello world
(`"Hello, World!\n";`), short-circuit `&&` / `||`, IEEE-754 `f32` / `f64`
arithmetic and comparison, recursive algorithms at `i32` / `i64` / `u64`
widths, and bidirectional integer / float literal narrowing all work and
are covered by the corpus.

The post-200 follow-up bundle (A.1–A.4) adds **`as` casts across the
full numeric matrix** (int↔int, int↔float, float↔float, with saturating
+ NaN→0 for float→int) and **class- and slice-typed values flowing
through fn signatures** via a hidden-out-pointer ABI. Helpers like
`fn doubled(c: Counter) -> Counter` and `fn print_slice(s: []u8) -> u0`
that previously had to be inlined at every call site now factor cleanly.

The Phase 13 bundle (B.1–B.5) reinstates LLVM as a parallel backend.
`arsenal build --backend=llvm path/to/foo.gw` now compiles and runs
all 226 corpus programs, with bit-exact agreement against Cranelift
on every exit code and every byte of stdout that has an
`.expected_stdout` file. The MIR is consumed unchanged by both
backends; the LLVM crate (`arsenal_codegen_llvm`) was a doc-comment
stub before B.1 and now carries roughly 950 LoC of compiler logic.
Bug yield across the bundle: zero — neither backend disagreed about
saturating fcvt, ordered float comparisons, sign-aware integer ops,
or the System V "memory class" aggregate ABI.

The Phase-2 entry brings c-strings (C.1 + C.2), `match` (M.1 +
M.2 + M.3), and `?T` (O.1) end-to-end. `c"..."` literals lex /
parse / typeck / MIR / Cranelift / LLVM; `[*:0]u8` is a
parsed-and-type-system-distinct sentinel pointer that decays to
`*u8` at extern call sites and at `let` annotations. `match`
accepts integer-literal, bool-literal, inclusive-range (`lo..=hi`),
or-pattern (`a | b | c`), and wildcard forms; arms unify against a
single result type, exhaustiveness requires either a `_` arm or
(for bool) both `true` + `false` arms. `?T` lands as a 2-field
`{tag: u8, payload: T}` aggregate (Phase-2 minimum: integer +
bool inners); `nil` types as `?T` in any Optional context; bare
`T` coerces to `?T` at assignable positions; `??` reads the tag
byte and lazily evaluates the RHS default on the nil branch.
The MIR-side decision-tree helper `lower_pattern_test` recurses
for or-patterns and emits short-circuit pairs for ranges; `??`
emits a 3-block tag-test CFG. Twelve new corpus programs (eight
match + three c-string + one `?T`) exercise the full Phase-2-entry
surface across both backends. Cleanup #1 dropped the
explicit-return-type requirement so unit-returning helpers
(`fn greet(s: [*:0]u8) { puts(s); }`) can elide `-> u0`.

**Phase 0 is complete. Phase 1 is functionally complete. Phase 13 is
complete. Phase 2 is in progress** (c-strings + match + `?T`
sub-bundles landed; the remaining sub-bundles are `?T` match
patterns (O.2), error unions `!T` + `!`-assert (O.3), and comptime
/ modules — see the [After Phase 1](#after-phase-1) section below).
The tactical-cleanup list under
[What doesn't work yet](#what-doesnt-work-yet-phase-1-deferred-or-incomplete)
shrinks accordingly.

---

## Where to start the next session

Read this whole document, then in priority order:

1. **`docs/spec.md` §5.3** (lexical structure) — refresher only.
2. **`docs/architecture.md` Part L Phase 1 deliverables** — the contract.
3. **`docs/architecture.md` Part B.3, C.3, D.1, F.1** — pipeline shape.
4. **The most recent commit message** (`git log -1`) — picks up the thread.
5. **`tests/snake_eater/pass/phase1/`** — skim a few `.gw` files to see
   what currently compiles and runs.

Then jump to **[After Phase 1 — 13 / Phase 2 / cleanup](#after-phase-1)** below.

---

## What's been built

```
gw/
├── docs/
│   ├── spec.md                  (input — language spec)
│   ├── architecture.md          (input — implementation contract)
│   ├── grammar.ebnf             (Phase 0; describes the parsed subset)
│   └── HANDOFF.md               (this file)
├── tests/snake_eater/
│   ├── pass/lexparse/           (61 .gw + insta snapshots — Phase 0)
│   ├── pass/phase1/             (226 .gw + .expected_exit / .expected_stdout)
│   └── fail/lexparse/           (5 .gw + .expected_diagnostics)
├── compiler/arsenal-boot/       (Cargo workspace — host = Rust 1.95+)
│   └── crates/
│       ├── arsenal_lex/         ★ active
│       ├── arsenal_ast/         ★ active
│       ├── arsenal_parse/       ★ active
│       ├── arsenal_resolve/     ★ active
│       ├── arsenal_typeck/      ★ active
│       ├── arsenal_mir/         ★ active
│       ├── arsenal_codegen_fast/★ active (Cranelift-backed)
│       ├── arsenal_codegen_llvm/★ active (LLVM-18-backed via inkwell, Phase 13)
│       ├── arsenal_driver/      ★ active (the `arsenal` binary)
│       ├── arsenal_borrow/        stub  (Phase 3)
│       ├── arsenal_lir/           stub  (Phase 7)
│       ├── arsenal_comptime/      stub  (Phase 2)
│       ├── arsenal_jit/           stub  (Phase 7)
│       ├── arsenal_lsp/           stub  (Phase 9)
│       ├── arsenal_fmt/           stub  (Phase 9)
│       ├── arsenal_doc/           stub  (Phase 9)
│       └── arsenal_cipher/        stub  (Phase 9 — package manager)
└── .github/workflows/ci.yml      (Linux/macOS/Windows matrix)
```

### Active crate roles (≈7 250 LoC of compiler logic)

| Crate | Phase | Role |
|---|---|---|
| `arsenal_lex` | 0 | UTF-8 lexer state machine. 108-variant `TokenKind`, phf keyword table, `Span`/`SourceMap`/`Diagnostic`/`DiagBag` types. |
| `arsenal_ast` | 0 | Hand-rolled rowan-style CST + typed AST. Single unified `SyntaxKind` enum (189 variants — `RangePat` added in M.3). Typed views for ~38 Phase-1 / Phase-2 node kinds; `Stub` variants for the rest. `Module::stmts()` exposes top-level stmts (11a). `CastExpr` typed view added in A.1. **`SentinelPtrType` typed view (C.2)** with `element()` + `sentinel()` accessors. **`Expr::Match` (M.1)** + `MatchExpr::scrutinee()` / `arms()`, `MatchArmList::arms()`, `MatchArm::pattern()` / `body()`. **`Pattern::Literal` (M.1) / `Range` (M.3) / `Or` (M.3)** promoted from `Stub`; views expose `value()` / `lo()` + `hi()` / `alternatives()` respectively. Bumpalo arena per file. Pretty-printer for `arsenal dump`. |
| `arsenal_parse` | 0 | Recursive-descent + Pratt expression precedence. Error-recovering. Produces both CST and AST. No parser generator. `parse_module` forks on `peek_item_keyword` between item and stmt (11a). `parse_type` handles `*T` / `[]T` / `&T` / `?T` / `[N]T` / **`[*:S]T` (C.2 — sentinel many-pointer; peek-at-1 of `Star` distinguishes from slice / array)**. **Postfix `as Type` (A.1)** at left binding power 22 — between `*`/`/`/`%` (19/20) and prefix unary (23), matching Rust precedence so `-1 as u32` parses as `(-1) as u32`. **Match (M.1–M.3)**: `parse_match_expr` invoked from `parse_primary` on `KwMatch`; scrutinee parsed with `struct_literals_allowed = false`. New `parse_match_pattern` separate from `parse_pattern` (used by `let` / `for in`) — match-arm patterns accept `_` / `Ident` / `IntLit` / `Minus IntLit` / `KwTrue` / `KwFalse` / `lo..=hi` / `a \| b \| c` chains; the literal-side parsing uses a custom `parse_pattern_literal_value` instead of `parse_expr` so `\|` (bp 9, bitwise OR) and `..=` stay available for the pattern grammar. Or-pattern wrapping uses `start_node_at` checkpoint; range-pattern wrapping uses the same trick. |
| `arsenal_resolve` | 1 | Walks the AST, registers top-level fn + class defs, exports `primitive_type_name()`. `DefKind::SyntheticMain` is registered when top-level stmts coexist without explicit `fn main` (11a). |
| `arsenal_typeck` | 1 / 2 | Bidirectional checker. `Ty` enum: `U0`/`Bool`/`Int(IntTy)`/`Float(FloatTy)`/`Rune`/`Class(DefId)`/`Slice(IntTy)`/`Ptr(IntTy)`/**`SentinelPtr { elem: IntTy, sentinel: u64 }` (C.2)**/**`Optional(OptInner)` (O.1) where `OptInner = Int(IntTy) \| Bool` is a closed enum**/`Error`. Emits a `TypedModule` with per-CST-node `expr_types`, `path_bindings`, `pat_bindings`, `call_targets`, `sigs`, `classes`. Slice + raw-pointer surface (11b/11c) are FFI-restricted; sentinel-pointer surface (C.2) is *not* — `[*:0]u8` flows through non-extern fn signatures because the producer-side sentinel guarantee gives the safety raw `*T` lacks. **Bidirectional literal narrowing (12d/12h)**: `check_expr` calls `try_narrow_literal` first — bare `IntLit`/`FloatLit`, `Unary(Minus, Literal)`, and `Paren(...)` shapes adopt the expected width when the value fits; out-of-range diagnoses against the literal span. `synth_binop_operands` extends the same rule across binary operators so `n < 2` (with `n: i64`) types cleanly. **`synth_cast` (A.1/A.2)** accepts the full numeric matrix `(Int\|Float, Int\|Float)`; non-numeric pairs reject with `UNSUPPORTED_CONSTRUCT`. **Class-/slice-typed fn params and returns (A.3/A.4)** are accepted via the by-pointer ABI; the `UNSUPPORTED_CONSTRUCT` rejections in `check_fn_signature` were dropped. **C.1 / C.2**: `synth_literal` types `c"..."` as `Ty::SentinelPtr { U8, 0 }`; `ty_assignable` adds the lone coercion `[*:S]T → *T` so the C.1 tracer's `puts(c"hi")` shape works without an explicit cast; missing return type defaults to `Ty::U0` (cleanup #1) instead of diagnosing — error code 307 is retired. **Match (M.1–M.3)**: `synth_match` synthesises the scrutinee, validates each arm's pattern via `check_match_pattern`, unifies arm bodies (first non-Error arm sets the result type, subsequent arms are checked against it). `check_match_pattern` accepts wildcards everywhere, integer-typed literal patterns + integer ranges (`Range`) when scrutinee is `Ty::Int(_)` (re-using the bidirectional narrowing for both bounds), `true`/`false` patterns when scrutinee is `Ty::Bool`, and `Or` patterns by recursing on each alternative. Exhaustiveness rule: every `match` requires either a `_` arm or — for bool scrutinees — both `true` and `false` literal patterns at top-level arms. Identifier patterns and other shapes still diagnose with UNSUPPORTED_CONSTRUCT until later widenings. **Optional (O.1)**: `Type::Opt(inner)` resolves to `Ty::Optional(OptInner)` when inner is integer/bool primitive (other inners reject); `try_narrow_literal` recognises `nil` in any `?T` context and adopts the expected Optional; `synth_literal` for `nil` outside an Optional context now diagnoses TYPE_MISMATCH (used to fall through silently to `Ty::Error` and pass any check); `ty_assignable` adds the lone `T → ?T` coercion edge — value-level distinct (the wrap below) but uniform at the source. Reverse direction (`?T → T`) is rejected; the user must unwrap. `synth_binary` dispatches `??` to `synth_coalesce`: LHS must be Optional, RHS checks against the inner, result type is the inner. |
| `arsenal_mir` | 1 / 2 | CFG of basic blocks; primitive locals + aggregate stack-slot locals (class + slice); `Assign`/`AssignField` statements; `Use`/`BinOp`/`UnOp`/`Field`/`Cast` rvalues; `Goto`/`Branch`/`Return`/`Call`/`Unreachable` terminators. Loop-target stack for break/continue. `lower_for` desugar. `Const::DataAddr` + program-level `string_literals` table for `.rodata` payloads (11b). Implicit Print at stmt-position string lits desugars to `write(1, slice.data, slice.len)`; auto-injects `extern fn write` if user didn't declare one (11c). **Short-circuit `&&` / `\|\|` (12b)**: `lower_short_circuit` emits a 3-block control-flow shape (rhs-eval / short-circuit / join) and bypasses `lower_binary` so the RHS is only evaluated when the LHS doesn't determine the result. **`Rvalue::Cast` (A.1/A.2)** carries `kind: CastKind`, `operand`, `src_ty`, `dst_ty`; the closed `CastKind` enum has 7 variants, each maps to one Cranelift op. `select_cast_kind` factors the kind selection out of `lower_cast`. **`def_to_fn` fix (A.3)**: pre-A.3 the map stored each def's position in `resolved.defs` (including class defs); A.3 only counts `Fn`/`SyntheticMain` defs when assigning indices, matching the order `functions` is populated. **C.1 / C.2**: `Const::CStrAddr(CStrLitId)` + program-level `cstring_literals` table parallel to `string_literals` (no shared dedup keys — slice payloads and c-string payloads carry different semantics). `lower_cstring_literal` interns the decoded bytes (no NUL terminator stored — codegen appends it) and returns the operand directly without materialising a slice aggregate. **Match (M.1–M.3)**: `lower_match` allocates `body_bb` + `next_bb` per arm, calls the recursive `lower_pattern_test` helper, lowers the body in `body_bb`, restores cursor to `next_bb` for the next arm. `lower_pattern_test` emits `Goto(body_bb)` for wildcards, `cmp = Eq; Branch` for literals, two short-circuit `Ge` / `Le` tests for inclusive ranges, and recursive chains (each alternative threads through a fresh `alt_next_bb`) for or-patterns. The chain-of-Branch shape is the same control flow already used by short-circuit `&&` / `\|\|`, so codegen needs zero new arms across the entire match sub-bundle. **Optional (O.1)**: new `let_ty_from_ast` helper resolves `?T` annotations so `lower_let` allocates the binding local at the correct Optional aggregate type. `wrap_to_optional_if_needed` materialises the implicit `T → ?T` coercion at let-init time — allocates a fresh aggregate temp, writes tag = 1 + payload via `AssignField`, returns `Operand::Local`. `lower_nil_literal` mirrors the shape for `nil`: tag = 0, no payload write (the tag distinguishes empty). `lower_coalesce` emits the 3-block decision: read tag → compare tag == 0 → `Branch` into nil-default-block (lazy RHS evaluation, assign result) or some-payload-block (read field 1 directly into result). Both arms `Goto` a shared join. |
| `arsenal_codegen_fast` | 1 / 2 | Cranelift-backed (placeholder until Phase 7 TPDE port). Aggregate (class + slice) layouts → stack slots; field reads/writes → stack_load/stack_store; aggregate-aggregate assigns → field-by-field copy. String literals materialised via `module.declare_data` + `define_data_object` under `__gw_str_<i>` symbols (11b). `*T` raw pointers lower as pointer-sized scalars (11c). **Float comparisons (12a)**: `lower_binop` branches on `ty.is_float()` for `Eq`/`Ne`/`Lt`/`Le`/`Gt`/`Ge` — floats use `fcmp` with the matching `FloatCC`, ints keep `icmp`. **Cast lowering (A.1/A.2)**: `Rvalue::Cast` arm reads operand at `clif_ty(src_ty)` and applies one Cranelift op per `CastKind` — `sextend`/`uextend`/`ireduce` for ints, `fcvt_from_sint`/`fcvt_from_uint` and saturating `fcvt_to_*_sat` for int↔float, `fpromote`/`fdemote` for floats. Same-width `*Bitcast` variants need no instruction. **Aggregate-by-pointer ABI (A.3/A.4)**: `make_signature` prepends a hidden out-pointer for aggregate returns and substitutes pointer-typed `AbiParam` for aggregate params. `define_fn` defers the entry-block switch until the lower-block loop's first iteration to keep Cranelift's "fill before switching" rule satisfied; aggregate params copy in via `copy_aggregate_from_ptr`, and `Terminator::Return` for an aggregate-returning fn copies out through `copy_aggregate_to_ptr`. `Terminator::Call` prepends `stack_addr(dst_slot)` for aggregate returns and substitutes `stack_addr` for aggregate args. **C.1**: parallel `__gw_cstr_<i>` rodata pass — payload is `bytes ++ "\0"`; `Const::CStrAddr` lowers via `module.declare_data_in_func` + `ins.global_value` exactly like `Const::DataAddr`. **C.2**: explicit `Ty::SentinelPtr { .. }` arms in `clif_ty` / `primitive_size_align` route to pointer-width — same shape as `Ty::Ptr`. **O.1**: `is_aggregate_ty` extended to include `Ty::Optional(_)`; `aggregate_layout` / `aggregate_field_ty` add `Optional` arms (tag at offset 0 / 1 byte; payload at the inner's natural alignment; total size aligned to inner align). Local-allocation site + `lower_assign_stmt`'s aggregate-dst branch now both go through `is_aggregate_ty` — fixed two inline `matches!(..., Class \| Slice)` patterns that silently routed Optional locals into the wrong storage and the wrong assign path (caught at the first dual-backend run). |
| `arsenal_codegen_llvm` | 13 / 2 | LLVM-18-backed via `inkwell` (B.1–B.5). Same `MirProgram → object bytes` contract as `arsenal_codegen_fast` — driver picks at `--backend=fast\|llvm`. Storage: alloca-per-local in the entry block (clang `-O0` style), `[N x i8]` allocas for aggregates with alignment bumped to the layout's max-field align via `InstructionValue::set_alignment`. Field addressing via byte-offset GEP through `i8` (opaque pointers; no struct types declared to LLVM). Bool stays at LLVM `i1` end-to-end (no i8 storage adapter). Float comparisons use ordered predicates (`OEQ`/`OLT`/etc.); float-→int casts route through the saturating `llvm.fpto{si,ui}.sat` intrinsics for Rust ≥ 1.45 / Cranelift parity. `Const::Float` lowers via `build_bit_cast(int_const, float_ty)` to preserve NaN payloads (a `const_float(f64)` round-trip would lose them on the F32 path). String literals materialise as one private `__gw_str_<i>` global per `MirProgram::string_literals` entry; `Const::DataAddr(id)` returns the global's address as `ptr`. Aggregate ABI: hidden out-pointer for aggregate returns; by-pointer for aggregate user params. `sret`/`byval` attributes intentionally omitted — corpus aggregates flow only between GW fns, plain-`ptr` agrees with Cranelift's manual `stack_addr` convention end-to-end. A small `build.rs` adds Homebrew's `lib` prefix to the linker search path on macOS so LLVM-18's system-libs (zstd, ffi, xml2, curses) resolve without `RUSTFLAGS` rituals. **C.1**: parallel pass for c-string globals — one private `__gw_cstr_<i>` per `MirProgram::cstring_literals` entry, payload `bytes ++ "\0"`; `Const::CStrAddr` returns the global's `as_pointer_value()`. **C.2**: explicit `Ty::SentinelPtr { .. }` arm in `llvm_basic_type` routes to opaque `ptr` — agrees with Cranelift's bit-exact output across all three c-string corpus programs. **O.1**: `is_aggregate_ty` / `aggregate_layout` / `aggregate_field_ty` extended for `Ty::Optional(_)` — same formula as Cranelift via the shared `optional_layout` helper, so the by-pointer ABI agrees byte-for-byte across backends. LLVM produced the correct 7 / 100 / 107 on first run; Cranelift did not (the inline-match bug above), and the dual-backend test surfaced the divergence immediately. |
| `arsenal_driver` | 0/1 | Subcommands: `arsenal new <name>`, `arsenal build [--backend=fast\|llvm] <file.gw>`, `arsenal dump <path>`, `arsenal --version`. Build pipeline: lex → parse → resolve → typeck → MIR → (Cranelift OR LLVM) → object → `cc` link → executable. `--backend=fast` is the default; both backends emit the same `Vec<u8>` object-bytes shape so the linker invocation is shared. |

---

## Phase 1 increment ledger

Each increment shipped one or more corpus programs and a single commit.

| # | Topic | Commit | Programs | New compiler code | Bugs caught |
|---|---|---|---|---|---|
| 1 | tracer bullet — `return 0` | `8051963` | +1 | scaffolded 4 stub crates as active + Cranelift wiring | scaffold |
| 2 | integer arithmetic | `dca994b` | +11 | (none) | **2** — literal-span over-extension; `Rvalue::BinOp::ty` storing result type instead of operand type |
| 3 | function calls | `fd5abac` | +5 | (none — already wired) | 0 |
| 4 | `let` locals | `058e9d6` | +6 | typeck `pat_bindings: NodePtr → BindingId` map | **2** — BindingId/Local mismatch when init exprs introduce temps; cli.rs CWD race in tests |
| 5 | `if`/`while` | `ec3ee35` | +11 | none structurally | **2** — U0-typed if-result-local crashed Cranelift; `else if` chain dropped from `IfExpr::else_branch` |
| 6 | bool / cmp | `b817dec` | +11 | none | 0 |
| 7+8 | loops & mutation (`=`, `break`, `continue`, `for in range`) | `cb796fe` | +14 | parser additions; typeck `loop_depth`; MIR `loop_targets` + `lower_for` desugar | 0 |
| 9 | extern fns + stdout tests | `84aa0eb` | +4 | `MirFn::is_extern`, `Linkage::Import` for externs; `.expected_stdout` infrastructure | **1** — silent miscompile: extern fns were getting defined as `udf` traps |
| 10 | classes (POD, locals only) | `c7870bb` | +8 | new SyntaxKind nodes (StructLit, Field), `Ty::Class`, `ClassLayout`, MIR `AssignField`/`Field`, codegen stack slots | 0 |
| 11a | top-level statements | `e52746f` | +5 | parse_module forks item/stmt; `Module::stmts()`; `DefKind::SyntheticMain`; check_synthetic_main_body; lower_synthetic_main with implicit `Return(0)` | 0 |
| 11b | `[]u8` slice type | `2545bb7` | +4 | `Ty::Slice(IntTy)`; `Const::DataAddr` + `string_literals` table; `lower_string_literal`; codegen `aggregate_layout` unifying class + slice; `module.declare_data` per literal | 0 |
| 11c | implicit Print + raw pointers | `0bf40f9` | +8 | parser PtrType arm; `Type::Ptr` AST view; `Ty::Ptr(IntTy)`; `slice.data: *u8`; pre-scan + `lower_implicit_print` desugar; auto-injected `extern fn write` | 0 |
| 12a | floating-point corpus | `e45723d` | +15 | codegen `lower_binop` adds `is_float` branch on every comparison op, emitting `fcmp` with the matching `FloatCC`; +3 codegen unit tests | **1** — `icmp` was unconditionally lowered for float comparisons → Cranelift verifier rejected |
| 12b | short-circuit `&&` / `\|\|` | `add7fe0` | +11 | MIR `lower_short_circuit` (3-block CFG: rhs-eval, short-circuit, join); `lower_expr` dispatches `AmpAmp`/`PipePipe` before `lower_binary`; +1 MIR unit test | **1** — `&&` and `\|\|` lowered as eager `band`/`bor`, observable via extern-call side effects |
| 12c | bitwise algorithms | `6fb3d45` | +12 | (none) | 0 |
| 12d | numerical fixtures + integer literal narrowing | `aa1536d` | +16 | typeck `try_narrow_literal` (bidirectional narrowing for `IntLit`/`FloatLit`); `synth_binop_operands` extends narrowing across binops; +5 typeck unit tests | **1** — integer literals stuck at `Ty::Int(I32)`; rejected wide-int corpus uniformly |
| 12e | class composition | `3543601` | +12 | (none) | 0 |
| 12f | slice + Print formatting | `3d91072` | +12 | (none) | 0 (but caught two corpus design rules: explicit return type required on every fn; `putchar`/implicit-Print don't share a buffer under piped stdout) |
| 12g | mixed extern fns | `42e17cc` | +10 | (none — adds `abs`/`getpid` corpus uses) | 0 |
| 12h | fill to 200 + negated literal narrowing | `8bc26a4` | +24 | typeck `try_narrow_literal` extended to recognise `Unary(Minus, Literal)` and `Paren(...)` shapes; +2 typeck unit tests | **1** — `if x == -100` (with `x: i64`) rejected because negated literals didn't narrow |
| A.1 | `as` cast int↔int | `c1b091e` | +8 | parser postfix `as Type` at BP 22; AST `CastExpr` view; typeck `synth_cast`; MIR `Rvalue::Cast` + `CastKind::{IntWiden,IntTrunc,IntBitcast}`; codegen `sextend`/`uextend`/`ireduce`/no-op; +6 typeck and +4 MIR unit tests | 0 |
| A.2 | `as` cast float bridge | `258cc70` | +6 | extends `synth_cast` to numeric ↔ numeric; `CastKind` adds `IntToFloat`/`FloatToInt`/`FloatExt`/`FloatTrunc`/`FloatBitcast`; codegen wires `fcvt_*`/`fpromote`/`fdemote` (saturating + NaN→0 for float→int); +3 net typeck and +7 MIR unit tests | 0 |
| A.3 | class-by-pointer ABI | `a6dc722` | +8 | typeck drops `UNSUPPORTED_CONSTRUCT` on class params/returns; codegen `make_signature` prepends hidden out-ptr for aggregate returns and substitutes ptr params for aggregate args; `copy_aggregate_from_ptr` at fn entry, `copy_aggregate_to_ptr` at return, `stack_addr` substitution at call sites; param prelude moved into the lower-block loop's iter-0 to satisfy Cranelift's "fill before switching" rule; +4 typeck and +1 MIR unit tests | **1** — latent `def_to_fn` off-by-N (counted class defs when assigning FnIdx); never triggered pre-A.3 because no class+fn-call combination existed |
| A.4 | slice-by-pointer ABI | `5d71372` | +4 | typeck drops `UNSUPPORTED_CONSTRUCT` on slice params/returns; **zero codegen changes** — `is_aggregate_ty` already covered `Ty::Slice`; +3 net typeck unit tests | 0 |
| B.1 | LLVM tracer bullet (`return 0` only) | `0c3a9fe` | (LLVM corpus 0 → 1) | `arsenal_codegen_llvm` from doc-comment stub to working `MirProgram → object bytes` via `inkwell`; `--backend=fast\|llvm` flag; `arsenal_codegen_llvm/build.rs` adds Homebrew lib paths on macOS for LLVM-18's system-libs (zstd / ffi / xml2 / curses); `arsenal_driver/tests/llvm_backend.rs` integration test; +1 integration test | 0 |
| B.2 | int + control flow + extern fns + recursion | `9384331` | (LLVM corpus 1 → 135) | alloca-per-local in entry block (clang `-O0` style); `Rvalue::Use`/`BinOp`/`UnOp` for ints + bools (signedness-aware Div/Mod/Shr); `Operand::Const(Int\|Bool\|Unit)` and `Operand::Local`; `Terminator::{Goto, Branch, Return, Call, Unreachable}`; bool stays at LLVM `i1` end-to-end so `Branch` needs no zext / trunc adapter | 0 |
| B.3 | float ops + `as` cast matrix | `9e6192c` | (LLVM corpus 135 → 168) | `Const::Float` via `build_bit_cast(int_const, float_ty)` (preserves NaN payloads); `lower_float_binop` uses ordered predicates (`OEQ`/`OLT`/etc.); `Rvalue::Cast` flat dispatch on `CastKind` (sext / zext / trunc / sitofp / uitofp / `llvm.fpto{si,ui}.sat` / fpext / fptrunc / no-op); intrinsic dispatch via `Intrinsic::find` + `get_declaration` per overload pair | 0 |
| B.4 | aggregate ABI (class + slice by-pointer) | `1129232` | (LLVM corpus 168 → 203) | aggregate locals: `[N x i8]` alloca with `set_alignment(layout.align)`; field addressing via byte-offset GEP through `i8`; aggregate Assign / Return / param entry copy via `llvm.memcpy`; `make_fn_type` prepends `ptr` for sret + substitutes `ptr` for aggregate args; `LoweringCx::ret_out_ptr` captured at fn entry; `lower_call` prepends `dst.alloca` for aggregate returns and substitutes `src.alloca` for aggregate args. `sret`/`byval` attributes intentionally omitted (no C-ABI consumers in Phase 1) | 0 |
| B.5 | string literals + Print desugar | `8c2a6df` | (LLVM corpus 203 → 226 — full parity) | private `__gw_str_<i>` global per `MirProgram::string_literals` entry, `Const::DataAddr(id) → global.as_pointer_value()`; `Ty::Ptr(_) → ptr` in `llvm_basic_type` and `make_fn_type` (extern `fn write(*u8, ...)` declares cleanly, `slice.data` loads back as `ptr`); empty-payload one-byte pad mirrors Cranelift; hand-curated `SUPPORTED` allow-list dropped in favour of iterate-the-corpus loop | 0 |
| cleanup #1 | default `-> u0` on missing return type | `e394571` | (no corpus add) | typeck `check_fn_signature` defaults the return type to `Ty::U0` instead of emitting MISSING_RETURN_TYPE (error code 307 retired); +2 typeck unit tests | 0 |
| C.1 | c-string tracer bullet | `1e8752c` | +1 (227) | typeck `synth_literal` for `CStringLit` returns `Ty::Ptr(IntTy::U8)` (provisional, retyped in C.2); MIR `Const::CStrAddr(CStrLitId)` + `MirProgram::cstring_literals` parallel table; `lower_cstring_literal` / `decode_cstring_literal` (delegates escape handling to the existing `decode_string_literal`); both backends gain a `__gw_cstr_<i>` rodata pass with `bytes ++ "\0"` payload; `Const::CStrAddr` lowers identically to `Const::DataAddr`; +2 typeck and +2 MIR unit tests | 0 |
| C.2 | `[*:0]u8` sentinel pointer type | `bd3cf5d` | +3 (228–230) | parser `[*:S]T` arm (peek-at-1 of `Star` distinguishes from slice / array); AST `Type::SentinelPtr(SentinelPtrType)` view promoted from `Stub`; `Ty::SentinelPtr { elem, sentinel }` (Phase 2 only realises `[*:0]u8`); `synth_literal` retypes `c"..."` from `*u8` to `[*:0]u8`; `ty_assignable` adds the lone coercion `[*:S]T → *T` so the C.1 tracer's `puts(c"hi")` shape works without explicit cast; both backends gain explicit `Ty::SentinelPtr { .. }` arms routing to pointer-width; +5 typeck unit tests | 0 |
| M.1 | match (literal int + wildcard) | `183e5b8` | +3 (231–233) | parser `parse_match_expr` invoked from `parse_primary` on `KwMatch`, scrutinee parsed with `struct_literals_allowed = false`; new `parse_match_pattern` separate from `parse_pattern` to keep `let 5 = …` rejected; `Expr::Match` + `Pattern::Literal` AST views promoted from `Stub`; typeck `synth_match` + `check_match_pattern` (integer-literal patterns + wildcard, exhaustiveness rule); MIR `lower_match` as chain of equality compares (one Eq + Branch per non-wildcard arm; wildcard contributes only a Goto); +5 typeck and +1 MIR unit tests | 0 |
| M.2 | bool match + statement-position match | `7d9c04d` | +2 (234–235) | parser `parse_match_pattern` accepts `KwTrue` / `KwFalse` as start tokens for `LiteralPat`; typeck `check_match_pattern` adds bool-scrutinee arm; `synth_match` tracks `has_true` / `has_false` so `match b { true => ..., false => ... }` is exhaustive without `_`; statement-position match works without further plumbing (existing `lower_expr_stmt → lower_expr → lower_match` path; `result_local` already short-circuits on `Ty::U0`); +4 typeck unit tests | 0 |
| M.3 | match range + or-patterns | `2d85e65` | +3 (236–238) | new `RangePat` SyntaxKind; AST `Pattern::Range(RangePat)` + `Pattern::Or(OrPat)` promoted from `Stub`; parser `parse_match_pattern` reads atoms separated by `\|` (wraps in `OrPat` via checkpoint), retroactively wraps a literal in `RangePat` if `..=` follows; new `parse_pattern_literal_value` helper avoids `parse_expr`'s Pratt operators stealing `\|` / `..=` from the pattern grammar; typeck `check_match_pattern` adds `Range` (integer scrutinee, both bounds narrow via `check_expr`) and `Or` (recurses on each alternative); MIR refactors into recursive `lower_pattern_test` helper — `Range` emits two short-circuit `Ge` + `Le` tests, `Or` chains alternatives through fresh `alt_next_bb`s, the helper centralises the "cursor ends at next_bb" invariant so `lower_match` shrinks; +5 typeck unit tests | 0 |
| O.1 | `?T` tracer (`Ty::Optional`, nil, `T → ?T`, `??`) | `7c46d5b` | +1 (239) | parser `??` infix at (16, 15) right-assoc; new `Ty::Optional(OptInner)` variant with closed `OptInner = Int(IntTy) \| Bool` enum; `resolve_type` for `Type::Opt` accepts integer / bool inners (rejects wider with UNSUPPORTED_CONSTRUCT); `try_narrow_literal` adopts `?T` for `nil` in any Optional context; `synth_literal` for `nil` outside Optional context now diagnoses TYPE_MISMATCH; `ty_assignable` adds the lone `T → ?T` coercion edge; new `synth_coalesce` (LHS Optional, RHS inner, result inner). MIR: `let_ty_from_ast` resolves `?T` so `lower_let` allocates the binding at the right type; `wrap_to_optional_if_needed` materialises the `T → ?T` coercion as a fresh aggregate temp (tag = 1, payload = T); `lower_nil_literal` writes only tag = 0; `lower_coalesce` emits a 3-block decision (read tag → compare = 0 → Branch nil-default vs payload-read). Both backends extended for `Ty::Optional(_)`: `is_aggregate_ty`, `aggregate_layout` (via shared `optional_layout` formula — tag at 0, payload at inner align, total aligned), `aggregate_field_ty` (u8 tag, inner payload). Cranelift's local-allocation + `lower_assign_stmt` paths both go through `is_aggregate_ty` now (fixing the predicted bug — see below); +8 typeck and +3 MIR unit tests | **1** — silent miscompile: Cranelift's local-allocation site and `lower_assign_stmt` aggregate-dst branch had inline `matches!(ty, Class \| Slice)` checks that didn't go through `is_aggregate_ty`. Optional locals routed into Variable storage instead of StackSlot, then the aggregate-Assign fell through to the primitive path — first run gave Cranelift exit 1 / 0 across the tracer's three smoke tests while LLVM gave the correct 7 / 100 / 107. Fixed by routing both sites through `is_aggregate_ty`; future aggregate variants are auto-handled |

**Key pattern**: each "0 bugs" increment was almost pure corpus growth (the
plumbing was already in place). Each "≥1 bug" increment caught real
miscompiles before they could compound. The tracer-bullet ordering paid off
visibly — every bug caught was 1 commit's worth of debugging instead of N+
commits' worth of "why is this wrong?"

In increment 12 the same rule held: 12a/12b/12d/12h each opened a new
*shape* through the pipeline (float comparisons → fcmp; eager-vs-lazy
boolean → control-flow lowering; literal default int → bidirectional
narrowing; bare-vs-negated literal narrowing) and each produced exactly
one bug. 12c/12e/12f/12g were recombinations of already-stressed
primitives and produced zero. The A.1–A.4 follow-up extended the same
ratio: A.3 was the only "new pipeline shape" sub-bundle (the by-pointer
calling convention) and yielded exactly one bug; A.1, A.2, A.4 were
recombinations and yielded zero. The heuristic is reliable enough to
use as a risk weighting when planning future bundles.

Phase 13 (B.1–B.5) is the one significant exception, and worth
recording. The pre-bundle prediction was *high* yield: B.3 (saturating
fcvt) and B.4 (aggregate ABI) were both shape-novel for a brand-new
backend, and "Cranelift / LLVM divergence" was explicitly the bundle's
selling point. Observed yield: zero across all five sub-bundles. The
result is itself the datapoint — at the surfaces Phase 1 exercises
(IEEE-754 ordered comparisons, sign-aware integer ops,
saturating + NaN→0 fcvt, System V "memory class" aggregate ABI),
LLVM 18 and Cranelift agree bit-exactly. The dual-backend test
starts paying its keep at Phase 2's shapes (comptime, larger corpus,
weirder values), not Phase 1's.

C.1 + C.2 + cleanup #1 also produced zero bugs across both backends.
Pre-bundle prediction by the 12/A.x heuristic was ~1 (the new `Ty`
variant + `[*:S]T → *T` coercion + new `Const::CStrAddr` MIR shape
were all shape-novel, and there was no reference oracle for c-string
typing). Observed yield: zero. The c-string surface is small enough,
and its value-level lowering identical enough across the two pointer
variants, that there was nowhere for a divergence to live. This
extends the Phase-13 pattern: when the *value-level* lowering of a
shape-novel feature is identical to an already-validated one (here:
`*T` and `[*:0]u8` both lower to opaque `ptr`), bug yield collapses
toward zero even when the *type-level* shape is new.

The match sub-bundle (M.1 + M.2 + M.3) repeats the result. Pre-bundle
prediction was ~2 across the three sub-bundles (M.1 introduces
decision-tree lowering; M.3 introduces inclusive-range bounds and
or-pattern alternation, both shape-novel for MIR). Observed yield:
zero. Same explanation: the *value-level* lowering reuses the
chain-of-Branch shape already validated by short-circuit `&&` / `||`
(12b), so the dual-backend test had nothing new to disagree about.

Phrased as an updated heuristic: weight future sub-bundles' predicted
bug yield by how much *value-level novelty* they introduce, not just
how much *surface novelty*. Phase 2's harder remaining sub-bundles
(`!T`/`?T`, comptime + modules) introduce new value-level shapes —
tag bytes for tagged unions, comptime evaluator state, multi-file
build orchestration — and should re-arm the prediction.

**O.1 confirms the refined heuristic.** `?T` introduces a 2-field
aggregate with a tag-byte read in the `??` operator's lowering —
genuinely new value-level surface. Pre-bundle prediction: ~1 bug.
Observed yield: 1 — and a *silent miscompile* at that, where
Cranelift's first run produced exit 1 (tracer should have produced
107) while LLVM correctly produced 107 from the start. The bug was
two inline `matches!(ty, Class \| Slice)` patterns that didn't go
through the `is_aggregate_ty` helper; Optional locals routed into
the wrong storage and the wrong assign path. The lesson: when a
new aggregate type variant lands, every place that switches on
"aggregate vs primitive" should go through one canonical
`is_aggregate_ty` predicate. Both Cranelift sites now do — future
aggregate variants (sum types, tagged unions, …) are
auto-handled.

### What 239 corpus programs cover

- Phase-0 syntax: every TokenKind variant, every operator precedence
  level, every supported statement form.
- Phase-1 semantics: integer arithmetic + comparison + bitwise + shift
  + logical ops on signed and unsigned integers; bool literals + `!`;
  function declarations with up to 2 params and i32 return; recursive
  calls (fib, fact); `let` with explicit and inferred types; `if`,
  `if/else`, `else if`, `while`, `break`, `continue`, `for x in 0..n`,
  `for x in 0..=n`, nested loops; assignment expressions; `extern fn`
  + stdout-comparison via libc `putchar` and libc `write`; classes
  with up to 3 fields, field read, field write, class fields driving
  control flow.
- Phase 1 increment 11 surface: top-level statements without `fn main`;
  implicit return-0 on fall-through; items + top-level stmts coexist;
  `[]u8` string slices via `let s: []u8 = "...";`; `slice.len`; the
  Phase-1 hello-world (`"Hello, World!\n";`) printing to stdout;
  multiple sequential Prints; Print inside `if`/`else` branches;
  string-literal escape decoding (`\n`, `\t`, `\\`, `\"`); empty
  Print; user-declared `extern fn write` reused by the Print desugar
  alongside manual `write(1, s.data, s.len)` calls.
- Phase 1 increment 12 surface: IEEE-754 `f32` / `f64` arithmetic and
  the full set of comparison operators (`==` / `!=` / `<` / `<=` / `>` /
  `>=`) with proper `fcmp` lowering; short-circuit `&&` / `||` with
  observable RHS skipping (extern calls, divide-by-zero patterns); a
  full bitwise algorithms suite (popcount, parity, byte pack/extract,
  nibble split, mask set/clear/toggle, power-of-two test, round-up,
  swap-via-xor, sign extraction, branchless abs, 8-bit reverse);
  numerical fixtures (fib, fact, Ackermann, Collatz, Euclidean GCD,
  integer sqrt, primality, integer power) at i32, i64, u64 widths;
  bidirectional integer / float literal narrowing in let initialisers,
  return values, call arguments, assignments, and binary operators
  (across positive, negated, and paren-wrapped literal shapes); class
  composition with multiple coexisting class types, classes carrying
  `f64` / `i64` fields, classes used as state machines across nested
  loops and as sources for extern-fn arguments; slice + Print
  formatting (multi-write output builders, recursive integer printing
  via putchar, padding, table rows, `[prefix][body][suffix]` write
  chains); mixed extern functions (`abs`, `getpid`) chained into
  arithmetic, into class fields, under short-circuit conditions, and
  in loop bounds.
- Phase 2 increment C.1 / C.2 surface: `c"..."` literals lex / parse /
  typeck / lower / codegen; `[*:0]u8` sentinel-pointer type as a
  parser-level distinct form, type-level distinct from `*u8`,
  value-level identical (both lower to opaque `ptr`); `[*:S]T → *T`
  decay at extern-call arg slots and at `let` annotations; c-string
  helper-fn fixtures with `[*:0]u8` flowing through a non-extern
  signature (uses cleanup #1's `-> u0` default to elide the return
  type); escapes round-trip (`\t`, `\\`, `\"`, etc.) decoded via the
  shared `decode_string_literal` after `c"` prefix strip.
- Phase 2 increment O.1 surface: `?T` optional type for primitive
  inners (`?i32`, `?i64`, `?bool`, …) at `let` annotations, fn
  signatures (param + return), and call-arg slots; `nil` literal
  adopts `?T` from any Optional context; bare `T` coerces to `?T`
  at any assignable position (let-init, return, call-arg);
  reverse direction (`?T → T`) rejected — user must unwrap. `??`
  coalesce operator: reads the LHS's tag byte, returns the
  payload if `tag == 1`, lazily evaluates the RHS default if
  `tag == 0`. The `?T` aggregate lowers as `{ tag: u8 @ 0,
  payload: T @ align_of(T) }`, total size aligned to the inner's
  alignment (`?i32` = 8 bytes, `?i64` = 16 bytes, `?bool` = 2
  bytes). Both backends agree byte-for-byte.
- Phase 2 increment M.1 / M.2 / M.3 surface: `match scrutinee {
  pattern => expr, ... }` at expression and statement position;
  pattern shapes accepted are wildcards (`_`), bare integer
  literals (`0`, `42`), negated integer literals (`-3`), boolean
  literals (`true` / `false`), inclusive ranges (`0..=9`), and
  top-level or-patterns chaining the above (`1 | 2 | 3`,
  `0..=9 | 100..=109`); exhaustiveness rule requires either a
  wildcard arm or — for bool scrutinees — both `true` and `false`
  literal patterns at top-level arms. Decision-tree lowering
  emits `cmp = Eq; Branch` per literal arm, two short-circuit
  `Ge` / `Le` compares per range arm, and recursive chains for
  or-patterns (each alternative tested in series, all sharing the
  same body block).
- Phase 1 follow-up A.1–A.4 surface: postfix `as Type` at Rust-style
  precedence; the full numeric cast matrix (int↔int with widen / trunc /
  signedness reinterpret; int↔float with signedness-aware fcvt;
  float↔float with promote/demote/identity); float→int saturation +
  NaN→0 (out-of-range positive clamps to dst::MAX, out-of-range
  negative clamps to dst::MIN, negative-to-unsigned clamps to 0);
  class-typed fn params and returns flowing through a hidden-out-pointer
  ABI (single class arg, multiple class args, multi-field classes,
  classes with `f64` fields, class-typed recursive calls); pass-by-value
  semantics for class params (callee mutations don't touch the caller's
  slot); slice-typed fn params and returns (factor `print_slice(s: []u8)`
  out of repeated `write(1, s.data, s.len)` chains); slice round-trip
  through both arg and return positions in the same call.

---

## What works (concretely)

```gw
class Counter { value: i32 }
extern fn putchar(c: i32) -> i32;
fn ack(m: i32, n: i32) -> i32 {
    if m == 0 { return n + 1; }
    if n == 0 { return ack(m - 1, 1); }
    return ack(m - 1, ack(m, n - 1));
}
fn main() -> i32 {
    let c = Counter { .value = 0 };
    for i in 1..=5 { c.value = c.value + i; }
    let mut_via_param: i32 = 0;
    while mut_via_param < 3 {
        putchar(65 + mut_via_param);
        mut_via_param = mut_via_param + 1;
        if mut_via_param == 99 { break; }
    }
    putchar(10);
    return c.value;
}
```

That program compiles and runs natively today. It exits with `15` after
printing `ABC\n`.

The Phase-1 hello-world is just one statement:

```gw
"Hello, World!\n";
```

No `main`, no extern declarations, no imports — the parser accepts the
top-level statement (11a), typeck assigns `[]u8` (11b), and the MIR
desugar emits a `write(1, str.data, str.len)` against an auto-injected
`extern fn write` (11c). Cranelift links it to libc's `write` symbol.

The Phase-2 c-string surface (C.1 + C.2) brings the canonical libc
shape:

```gw
extern fn puts(s: *u8) -> i32;

fn greet(s: [*:0]u8) {
    puts(s);
}

fn main() -> i32 {
    greet(c"first");
    greet(c"second");
    return 0;
}
```

`c"..."` types as `[*:0]u8`, the sentinel-pointer type decays to
`*u8` at the `puts(s)` call site, the helper fn `greet` elides its
return type via cleanup #1's `-> u0` default. Both backends compile
this program to the same `first\nsecond\n` output.

The Phase-2 match surface (M.1 + M.2 + M.3) brings every supported
pattern shape together:

```gw
fn classify(x: i32) -> i32 {
    return match x {
        0..=9 | 100..=109 => 1,
        50 | 60 | 70 => 2,
        -1 => 3,
        _ => 0,
    };
}

fn main() -> i32 {
    return classify(105);
}
```

Exit code: 1. The match desugars to a chain of compare+branch
sequences — two range tests (each two compares) for the first arm,
three equality tests for the second, one equality test for `-1`,
and a final `Goto` for the wildcard. Both backends produce
bit-exactly the same value across all 239 corpus programs.

The Phase-2 `?T` surface (O.1) brings the canonical optional shape:

```gw
fn main() -> i32 {
    let x: ?i32 = 7;
    let y: ?i32 = nil;
    return (x ?? 0) + (y ?? 100);
}
```

Exit code: 107. `7` wraps into `{tag: 1, payload: 7}`, `nil` lowers
as `{tag: 0, ...}`, the two `??` reads check tag bytes and lazily
pick payload-or-default. Both backends produce 107 byte-for-byte.

### Driver UX

```bash
$ arsenal new hello
created project `hello`:
  hello/MotherBase.gw       # Phase 2 manifest (currently has Phase-2 syntax)
  hello/hello.gw            # spec §5.15.1 hello world (currently rejected)
$ arsenal build path/to/some.gw
built `path/to/some`
$ ./path/to/some
$ echo $?
21
$ arsenal build --backend=llvm path/to/some.gw   # Phase 13
built `path/to/some`
$ arsenal dump path/to/some.gw     # AST dump for debugging
$ arsenal --version
arsenal 0.0.1
```

### Test infrastructure

- `cargo test` at workspace root runs the entire suite (148 tests).
- `cargo test -p arsenal_parse --test snake_eater` runs the lex+parse
  insta snapshot corpus (61 pass, 5 fail).
- `cargo test -p arsenal_driver --test phase1_run` runs every
  `tests/snake_eater/pass/phase1/*.gw` end-to-end through the
  Cranelift backend: builds, executes, matches exit code (and stdout
  where `.expected_stdout` is present). Skipped on Windows
  (`#![cfg(not(windows))]`) — `cc` integration is a later concern.
- `cargo test -p arsenal_driver --test llvm_backend` runs the **same
  226-program corpus** through `arsenal build --backend=llvm`. Both
  tests share the corpus directory; any program added to
  `tests/snake_eater/pass/phase1/` is automatically exercised through
  both backends. Requires `LLVM_SYS_180_PREFIX` set at build time
  (see Pre-flight checklist).
- CI workflow at `.github/workflows/ci.yml` runs build + fmt --check +
  clippy `-D warnings` + test on Linux / macOS. The matrix installs
  LLVM 18 via each runner's native package manager (`brew install
  llvm@18` on macOS; `apt.llvm.org/llvm.sh 18 all` on Linux) and
  exports `LLVM_SYS_180_PREFIX`, so the full workspace — including
  the `llvm_backend` integration test — runs on every push to main
  and every PR. Windows is intentionally absent: `llvm-sys 180`
  needs the LLVM 18 dev libraries, which lack a usable distribution
  path on Windows (Choco's `llvm` is a clang+lld user toolchain, not
  a dev install). Restore Windows when either `arsenal_codegen_llvm`
  is feature-gated or a working Windows install path emerges.

---

## What doesn't work yet (Phase-1-deferred or incomplete)

| Limitation | Surface | Path forward |
|---|---|---|
| Raw pointers outside `extern fn` signatures | Typeck rejects `*T` in non-extern fn params/returns | Memory-model + borrow-checker work (Phase 3); also blocks meaningful pointer arithmetic |
| Nested class fields | Typeck rejects | Generalise size/offset computation in `resolve_class_layout`; recurse on `Ty::Class` field types |
| Slice-typed class fields | Typeck rejects | Class layout would need to embed the slice's `(data, len)` pair |
| Non-`u8` slice element types | Typeck rejects `[]i32` etc. (only `[]u8` accepted today) | Generalise the slice arm in `resolve_type`; aggregate_layout already handles arbitrary 8-byte fields, so codegen mostly follows |
| `match`, error unions (`!T`), generics, `cipher`, async, comptime | Parser produces `ErrorNode`s | Phases 2–4 |
| Multi-segment paths in expressions (`std::mem::Foo`) | Typeck `UNSUPPORTED_CONSTRUCT` | Phase 2 (frequencies / module imports) |
| Slice slicing (`s[1..3]`), array-to-slice coercion | No syntax / typing rules yet | Phase 2 |
| Pointer arithmetic, dereference (`*p`), address-of (`&x`) | No syntax / typing rules yet | Phase 3 with the memory model |
| Mixing `putchar` and implicit Print in the same program | Output ordering under piped stdout is `[all writes][all putchars]` because stdio buffers putchar but `write(2)` syscall bypasses stdio | Add an `extern fn fflush(stream: *u8) -> i32;` corpus pattern, OR document the rule (current state — see corpus design notes below) |
| `BinOp::Mod` and `BinOp::Pow` on float operands | Codegen falls through to `srem`/`urem` (wrong) or traps (Pow) | Typeck doesn't currently produce float `%` / `**`. If a future corpus does, add float arms in `lower_binop` (both backends now have a stub Unsupported / trap path) |
| `arsenal new` template parses cleanly | Templates use `#virtuous {}` syntax that Phase 1 parser rejects | Swap templates to Phase-1 syntax (the bare-string-literal half now works after 11c, but the `#virtuous` directive is still rejected) |
| Windows CI coverage | `arsenal_codegen_llvm`'s `llvm-sys 180` dep can't be satisfied on Windows runners (no usable dev install path); Windows is dropped from the CI matrix | Either (a) feature-gate `arsenal_codegen_llvm` so Windows builds the rest of the workspace without it, or (b) find / build an llvm-sys-compatible LLVM 18 distribution for Windows. Until then, fmt / clippy / build / test all run only on Linux + macOS |
| Class field of type `bool` | Loads / stores at LLVM's `i1` width into a `(1, 1)` byte slot | No corpus program currently exercises this. If one shows up the fix is the standard zext-on-store / trunc-on-load adapter (matches the `i8`-storage convention rustc uses) |

### Corpus design notes (rules learned during increment 12 / A.x)

These don't reflect compiler bugs — they're properties of the current
Phase-1 surface that any future corpus author needs to know.

1. **Don't mix `putchar` with implicit Print** in the same program if
   `.expected_stdout` matters. `putchar` writes through libc's stdio
   buffer; the implicit Print desugar uses a direct `write(1, …)`
   syscall. Under the piped stdout of `phase1_run`, stdio is fully
   buffered, so all `write` calls flush immediately while all `putchar`
   calls accumulate until exit — the recorded order is
   `[all writes][all putchars]`, not source order. Either commit to one
   mechanism per program, or use only `write` (the implicit Print and
   user-declared `extern fn write(…)` share the same kernel-side path).
2. **Every `fn` declaration needs an explicit `-> T`.** There's no
   implicit `-> u0` arm in the parser. Helpers that do I/O without a
   meaningful return value should be written as `fn print_x(…) -> u0`.
3. **Exit codes are 8-bit (POSIX).** Programs that compute a sum > 255
   and return it observe `result % 256` as the exit code. Either keep
   sums small or check the value via `if r == EXPECTED { return
   SOME_SMALL_I32; }` (the standard pattern across most of the
   wide-int and float corpus).
4. **`as` is a *value* cast, not a *bounds* check.** Narrowing int casts
   silently truncate (low bits) and narrowing float→int casts saturate
   to dst min/max (NaN → 0). Both match Rust ≥ 1.45. If the corpus
   program needs a check, write it explicitly (`if x > MAX { … }`)
   before the cast.
5. **Aggregate fn-signature ABI is by-pointer** (A.3/A.4). Class- and
   slice-typed params lower to a hidden pointer; aggregate returns
   prepend a hidden out-pointer to the arg list. Pass-by-value
   semantics still hold from the source's perspective — `copy_aggregate
   _from_ptr` at fn entry materialises a fresh copy in the callee's
   local slot. The cost is the entry copy plus the field-by-field
   return store; cheap for Phase-1-sized aggregates, irrelevant once
   the TPDE backend lands.
6. **`[*:0]u8` and `*u8` are type-distinct, value-identical** (C.1/C.2).
   `c"..."` literals are `[*:0]u8`; the producer side guarantees the
   trailing NUL. The lone `[*:S]T → *T` coercion lives in
   `ty_assignable` so existing `extern fn x(*u8)` slots accept
   c-string args without explicit casts. There is no reverse
   coercion: a `*u8` you got from `slice.data` does *not* type as
   `[*:0]u8` (the sentinel guarantee isn't there). The Phase-1
   FFI-only restriction on raw `*T` (decision #13) does *not*
   extend to `[*:0]u8` — sentinel-pointer params and locals are
   permitted in non-extern fns because the producer-callee contract
   gives the safety raw `*T` lacks.

---

## Known design decisions worth re-confirming next session

These are user-approved choices that affect ongoing work. Re-confirm at
session start before changing them.

1. **Tracer-bullet ordering**: each Phase-1 increment is end-to-end
   compileable + runnable, never "build subsystem N to completion then
   subsystem N+1". *(approved at start of Phase 1)*
2. **Cranelift and LLVM ship as parallel backends** (Phase 13 / B.1–B.5)
   — `arsenal build --backend=fast` (Cranelift, default) and
   `--backend=llvm` (LLVM 18 via inkwell) both compile the entire
   226-program corpus. Both consume the same `MirProgram`. LLVM is
   pinned to 18.x (inkwell 0.5 + `llvm-sys 180`); upgrading the
   feature flag in `[workspace.dependencies]` is a coordinated change
   to `arsenal_codegen_llvm/src/lib.rs` (intrinsic names + opaque-
   pointer assumptions). Architecture Part F.2 is now satisfied —
   LLVM is the architecture-mandated backend, Cranelift remains
   because it's the placeholder for the Phase 7 TPDE port.
3. **`cc` for linking, not bundled `lld`** — architecture wants lld
   eventually (Part J.3); Phase 1 shells out to system `cc` (clang on
   macOS, gcc on Linux). Windows linker untested.
4. **Multi-session Phase 1** — we explicitly do not aim to land all of
   Phase 1 in a single session. Each commit is shippable in isolation.
5. **`let` is mutable in Phase 1** — spec §5.3 says `let`/`var` distinguish
   immutable/mutable, but Phase 1 typeck accepts assignment to any
   let-binding. The check is a Phase 3 borrow-checker concern.
6. **Class struct-literal syntax: `Foo { .x = 1, .y = 2 }`** — leading-dot
   field syntax per spec §5.15.2.
7. **Struct literals are disallowed in `if`/`while`/`for` conditions** —
   `parser.struct_literals_allowed` flag; user works around with parens
   or temporary lets. Same trick rustc uses.
8. **`MirStmt::AssignField` is a flat statement** — Phase 1 doesn't
   model nested `Place` projections. Users break `a.b.c` chains with
   intermediate bindings.
9. **Test cwd race fixed via mutex in `cli.rs` test file** — not a real
   compiler issue but a test-harness one. Don't lose this when adding
   new tests that touch `set_current_dir`.
10. **Synthesised `main` symbol** (11a) — top-level statements lower to a
    `main` linker symbol (not `_start`); avoids replacing crt0 so libc
    fns like `putchar`/`write` keep working.
11. **Slice as synthetic 2-field aggregate** (11b) — a slice value lives
    in a stack slot with `data: ptr@0, len: usize@ptr_bytes`, riding the
    same `MirStmt::AssignField` / `Rvalue::Field` machinery as classes.
    No separate "slice operand" abstraction.
12. **Auto-injected `extern fn write`** (11c) — implicit Print desugars
    to a `write(1, slice.data, slice.len)` call. If the user already
    declared `extern fn write` we reuse their FnIdx; otherwise we
    synthesise an extern decl with the libc signature
    `(i32, *u8, usize) -> isize`.
13. **Raw pointers FFI-only** (11c) — `*T` is parseable and accepted in
    extern fn signatures; non-extern fn params/returns reject it with
    `UNSUPPORTED_CONSTRUCT`. Phase 1 only allows `*u8` / `*i8`.
14. **Float comparisons via `fcmp`** (12a) — codegen `lower_binop`
    branches on operand `ty.is_float()` for every comparison op and
    emits `fcmp` with the `FloatCC` matching the syntactic operator
    (`Equal` / `NotEqual` / `LessThan` / etc). The integer path keeps
    `icmp`. Cranelift's ordered comparisons match user expectation of
    `==` / `<` / etc. on non-NaN floats; NaN handling falls out cleanly
    because ordered comparisons return false against NaN.
15. **Short-circuit `&&` / `||` are control-flow, not BinOps** (12b) —
    MIR `lower_short_circuit` emits a 3-block CFG (rhs-eval block, a
    short-circuit block that assigns the determined constant, a join
    block) and assigns into a single bool result local. RHS is only
    evaluated on the take-branch. The `BinOp::LogAnd` / `LogOr` enum
    variants and their `band`/`bor` codegen arms remain in place but
    are never emitted by lowering; they're dead code kept for enum
    symmetry.
16. **Bidirectional integer/float literal narrowing** (12d/12h) — the
    typeck's `check_expr` calls `try_narrow_literal` first; bare
    `IntLit` / `FloatLit`, `Unary(Minus, Literal)`, and `Paren(...)`
    shapes adopt the expected width when the value fits. Out-of-range
    integer values diagnose against the literal span with the requested
    type named. `synth_binop_operands` extends the same rule across
    binary operators so `n < 2` (with `n: i64`) types cleanly without
    the user inserting an `as` cast (which doesn't exist anyway).
    Integer bounds are checked via `value_fits_int` against per-`IntTy`
    ranges; `ISize`/`USize` use 64-bit limits as a Phase-1
    simplification (revisit when a 32-bit target ships).
17. **Phase-1 corpus target met** (12h) — 200 `.gw` programs, all
    compile and run, all match expected exit code and stdout. Any
    further corpus growth should be motivated by a specific bug
    suspicion or a newly-supported construct (A.1–A.4 added 26 such
    programs against the new `as` and aggregate-ABI surfaces).
18. **`as` precedence: Rust-style** (A.1) — postfix `as Type` at left
    binding power 22, between `*`/`/`/`%` (19/20) and prefix unary
    (23). So `a * b as T` parses as `a * (b as T)`, `-1 as u32` as
    `(-1) as u32`, `2 ** 3 as i64` as `2 ** (3 as i64)`. Same as Rust.
19. **`as` cast semantics** (A.1/A.2) — int↔int narrowing **silently
    truncates** the low bits (Rust / Zig `@truncate` style; the user
    opted in by writing `as`). Same-width signedness reinterpret is a
    no-op since Cranelift integer types don't carry signedness. Float
    →int conversions are **saturating + NaN→0** (matches Rust ≥ 1.45):
    out-of-range positive clamps to `dst::MAX`, out-of-range negative
    to `dst::MIN`, NaN to `0`. Cranelift's `fcvt_to_*_sat` ops do this
    natively — no NaN-detection branch in our generated code.
20. **`CastKind` is a closed enum** (A.1/A.2) — `IntWiden { signed }`,
    `IntTrunc`, `IntBitcast`, `IntToFloat { signed }`, `FloatToInt
    { signed }`, `FloatExt`, `FloatTrunc`, `FloatBitcast`. Each maps
    to exactly one Cranelift op (or no op for the `*Bitcast` arms).
    `signed` tracks the **operand**'s signedness for `IntWiden` and
    `IntToFloat`, the **destination**'s signedness for `FloatToInt`.
    `select_cast_kind(src_ty, dst_ty)` factors the dispatch out of the
    builder so it's testable in isolation.
21. **Aggregate ABI: hidden out-pointer + by-pointer args** (A.3/A.4)
    — System V's "memory class" rule applied uniformly: every aggregate
    return (class or slice) prepends an extra `*ptr` parameter; every
    aggregate user param substitutes a `*ptr` for the value. The
    "split into two registers" optimisation for ≤ 16-byte aggregates
    is **deliberately deferred**; the by-pointer-always rule keeps
    codegen flat and is invisible at the GW source level. Caller
    obtains addresses via `stack_addr(slot, 0)`. The `fn` returns
    `void` at the Cranelift level when the GW-level return is
    aggregate.
22. **Aggregate param prelude lives inside `lower_block` iter 0**
    (A.3) — Cranelift's frontend rejects `switch_to_block` on an
    unfilled block, even when switching to the same block. Pre-A.3
    the upfront `switch_to_block(entry)` worked because only block-
    params + `def_var` were emitted (no instructions). A.3's aggregate
    copy-in emits load+store, which would trip the rule. Resolution:
    don't pre-switch; let the lower-block loop do the single switch
    per block, with iteration 0 emitting the param prelude inline
    after the switch. The hidden out-pointer (when present) is
    captured into `LoweringCx::ret_out_ptr` for `Terminator::Return`
    to copy through.
23. **`def_to_fn` only counts fn-shaped defs** (A.3) — pre-A.3 the
    map stored each def's position in `resolved.defs` directly. Class
    defs share the same vector but never appear in
    `MirProgram::functions`, so a class declared before a fn shifted
    every subsequent fn's FnIdx by one. The bug was latent because
    pre-A.3 typeck rejected class-typed params/returns, so no Call
    terminator ever dispatched to a fn defined after a class. A.3
    surfaces it; the fix only increments the FnIdx counter for `Fn` /
    `SyntheticMain` defs.
24. **Backend selection is a CLI flag, not a feature** (B.1) — the
    `--backend=fast|llvm` flag in `cmd_build.rs` dispatches to
    either `arsenal_codegen_fast::compile_program` or
    `arsenal_codegen_llvm::compile_program`. Both crates are
    unconditional workspace dependencies; there's no `cfg` gate on
    LLVM. Building the workspace requires LLVM 18 to be installed
    (see #25). Default is `fast` so `arsenal build foo.gw` keeps
    behaving as before. Naming reflects the crate names — `fast`
    survives the eventual TPDE swap inside `arsenal_codegen_fast`
    without a rename.
25. **LLVM 18 build prerequisites** (B.1) — the workspace needs
    `LLVM_SYS_180_PREFIX` set when invoking `cargo build` /
    `cargo test`. On macOS: `brew install llvm@18` and
    `export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18`. On Linux:
    install LLVM 18 dev libs from the official LLVM apt/yum repo
    (Ubuntu's bundled `llvm-dev` may be too old) and set
    `LLVM_SYS_180_PREFIX` to its prefix. Additionally, LLVM 18's
    system-libs (zstd / ffi / xml2 / curses) must be linker-findable;
    `arsenal_codegen_llvm/build.rs` adds `/opt/homebrew/lib` and
    `/usr/local/lib` on macOS so Homebrew's keg-only `zstd` etc.
    resolve without `RUSTFLAGS` rituals.
26. **LLVM aggregate ABI: plain `ptr`, no `sret`/`byval` attrs** (B.4)
    — `arsenal_codegen_llvm::make_fn_type` emits a hidden `ptr` for
    aggregate returns and `ptr` for aggregate user params, with no
    `sret(<type>)` / `byval(<type>)` parameter attributes attached.
    This is sufficient because corpus aggregates flow only between GW
    fns, never through C ABI; the plain-`ptr` form agrees bit-exactly
    with Cranelift's manual `stack_addr` convention across the
    corpus. **If Phase 2+ ever passes an aggregate to a C extern**
    (extending the corpus or adding a real FFI surface), add `sret` /
    `byval` then; both inkwell methods exist and the codegen call is
    a one-line addition per arm.
27. **Bool stays at LLVM `i1` end-to-end** (B.2) — alloca `i1`,
    store `i1`, load `i1`, branch on `i1`. No `i8` storage adapter
    (clang / rustc use `i8` storage for ABI compliance with C
    `_Bool`). The decision keeps the lowering code uniform but means
    a class field of type `bool` would store/load at `i1` width
    against a 1-byte slot — works on x86_64 / aarch64-apple-darwin
    (which tolerate misaligned 1-bit access) but isn't strictly C-
    ABI-compliant. No corpus program currently has a bool class field;
    if one shows up, switch to the zext-on-store / trunc-on-load
    adapter.
28. **One LLVM `Context` per `compile_program` call** (B.1) — every
    `arsenal build --backend=llvm` invocation creates a fresh
    `inkwell::context::Context`, builds the module, emits the object,
    drops the context. There's no cross-call context reuse. This is
    the reason the LLVM corpus test takes ~30s for 226 programs —
    LLVM target init dominates. Once we batch-compile in Phase 2 (a
    single `cargo build` of a multi-file project), share one context
    across the whole build invocation. For one-shot `arsenal build
    foo.gw` the per-call cost is unimportant.
29. **Default `-> u0` on missing fn return type** (cleanup #1) —
    typeck `check_fn_signature` defaults the return type to
    `Ty::U0` when the source omits `-> T`, instead of emitting
    error 307 (now retired). `fn helper(c: Counter) { puts(c"x"); }`
    is well-typed; `return 1;` from such a fn still diagnoses
    against the inferred `u0` via the existing
    RETURN_VALUE_MISMATCH path. The parser already accepted the
    optional `RetType`; only the typeck rejection moved.
30. **C-string literals: parallel MIR table, not a flag** (C.1) —
    `MirProgram::cstring_literals: Vec<Vec<u8>>` lives next to
    `string_literals` rather than the two sharing one table with
    an `is_cstring` bool. Reasons: (a) the dedup keys differ —
    a `"hi"` slice payload and a `c"hi"` payload are different
    values even though their bytes overlap; (b) slice consumers
    read the byte length to materialise the slice's `len` field,
    while c-string consumers don't, and crosstalk would force
    every reader to learn both shapes; (c) a new `Const::CStrAddr`
    variant is self-documenting at every use site. Codegen mirror
    the layout: parallel `__gw_cstr_<i>` rodata pass in both
    backends, `bytes ++ "\0"` payload, identical lowering of
    `Const::CStrAddr` and `Const::DataAddr` modulo the global they
    point at. The empty-payload one-byte pad is unnecessary for
    c-strings (the appended NUL guarantees ≥1 byte).
31. **`Ty::SentinelPtr` decays only to `*T`, never to `[]T`** (C.2)
    — `ty_assignable` adds exactly one new edge:
    `Ty::SentinelPtr { elem: e1, .. } → Ty::Ptr(e2)` when `e1 ==
    e2`. There is no reverse coercion (a `*u8` you got from
    `slice.data` lacks the sentinel guarantee), no
    `[*:S]T → []T` decay (slices have a length, c-strings don't),
    and no implicit `[*:0]u8 → [*:0]u8` widening across element
    types (Phase 2 only realises `[*:0]u8` anyway). Phase 2
    explicitly accepts only `[*:0]u8` at `resolve_type`; other
    sentinels and other element types diagnose with
    UNSUPPORTED_CONSTRUCT, naming the rejected shape so users see
    *why* their type didn't take. `Ty::SentinelPtr` does NOT
    inherit the FFI-only restriction on `Ty::Ptr` (decision #13)
    — it flows freely through non-extern fn signatures because
    the producer-side sentinel terminator gives the safety raw
    `*T` lacks. Both backends route `Ty::SentinelPtr { .. }` to
    pointer-width / opaque `ptr` via explicit arms in `clif_ty`
    / `llvm_basic_type` / `primitive_size_align`; MIR sees no
    new shape (the Operand path is just `Const::CStrAddr`).
32. **Match patterns parse with a custom literal helper, not
    `parse_expr`** (M.1 / M.3) — `parse_match_pattern_atom` calls a
    new `parse_pattern_literal_value` that emits exactly one of
    `IntLit` / `Minus IntLit` / `KwTrue` / `KwFalse`, with no Pratt
    operators in the middle. Reusing `parse_expr` would let `|`
    (bp 9, bitwise OR) and `..=` (range op) fall inside the literal
    expression's parse tree, stealing the alternation token from
    the pattern grammar (`a | b | c`) and the range token from the
    range-pattern wrapper (`0..=9`). Two parsers are kept separate
    for the same reason `parse_match_pattern` is separate from
    `parse_pattern` (used by `let` / `for in`): widening one
    surface shouldn't silently widen the other.
33. **Match exhaustiveness rule** (M.1 / M.2) — every `match`
    requires either a `_` arm OR (for `Ty::Bool` scrutinees) both
    `true` and `false` literal patterns at top-level arms. Integer
    scrutinees always need a wildcard because the domain is too
    large to enumerate; bool's two-value domain accepts the
    explicit-coverage form. Or-patterns of literals (`true | false
    => …` as a single arm) are *not* counted toward bool
    exhaustiveness in M.3 — the user can write `_ =>` if they want
    one-arm coverage. Identifier patterns and other non-literal /
    non-wildcard / non-range / non-or patterns still diagnose
    UNSUPPORTED_CONSTRUCT until later widenings.
34. **Match decision-tree lowering via `lower_pattern_test`**
    (M.1 / M.3) — each arm allocates `body_bb` + `next_bb`; the
    helper emits whatever comparison/branch shape the pattern
    needs and leaves the cursor at `next_bb` so the next arm's
    test starts cleanly. `Wildcard` → `Goto(body_bb)`. `Literal`
    → `cmp = Eq; Branch`. `Range` → two short-circuit `Ge` / `Le`
    tests through a fresh `hi_test_bb`. `Or` → recursive: each
    alternative threads through its own `alt_next_bb`, with the
    last alternative's miss flowing to the arm's overall
    `next_bb`. Codegen needs zero new arms across the entire
    match sub-bundle because the chain-of-Branch shape is what
    short-circuit `&&` / `||` (12b) already exercised.
35. **`?T` lowers as `{tag: u8 @ 0, payload: T @ align_of(T)}`**
    (O.1) — tag byte at offset 0; payload at the inner's natural
    alignment (which is also the aggregate's alignment); total
    size aligned up to the inner align. So `?i32` is 8 bytes,
    `?i64` is 16, `?bool` is 2. The closed `OptInner = Int(IntTy)
    | Bool` enum keeps `Ty: Copy`. Phase-2 minimum supports
    integer + bool inners only (other inners diagnose with
    UNSUPPORTED_CONSTRUCT, naming the rejected shape so users
    see *why* their type didn't take). Wider inners (classes,
    slices, pointers, nested Optionals) ride later sub-bundles.
    Rides the existing class-by-pointer ABI (A.3 / A.4) for free
    at fn signatures.
36. **`T → ?T` coerces; `?T → T` does not** (O.1) — `ty_assignable`
    adds exactly one new edge (`Ty::Optional(inner) ← inner`).
    The reverse direction is rejected; the user must unwrap via
    `??` or (later) `match` / `!`. The MIR-level wrap
    (`wrap_to_optional_if_needed`) materialises the value-level
    coercion: allocate aggregate temp, write tag = 1 + payload =
    T, return `Operand::Local`. `nil` adopts the expected
    Optional in `try_narrow_literal`; outside an Optional context
    `synth_literal` diagnoses TYPE_MISMATCH for `nil` rather than
    silently falling through to `Ty::Error`.
37. **`??` infix at (16, 15), right-associative** (O.1) —
    tighter than logical / comparison / bitwise, looser than
    arithmetic. `a ?? b ?? c` chains as `a ?? (b ?? c)`;
    `x ?? 0 == 5` parses as `(x ?? 0) == 5`. RHS is lowered
    *lazily* — only on the nil branch — matching the
    short-circuit shape of `&&` / `||`. The 3-block CFG (read
    tag, branch, nil-default-block / some-payload-block,
    shared join) is independent of the inner type's lowering.
38. **`is_aggregate_ty` is the canonical aggregate predicate**
    (O.1 lesson) — every site that switches on
    "aggregate vs primitive" must go through `is_aggregate_ty`,
    never inline `matches!(ty, Class | Slice)` checks. The O.1
    Cranelift bug was exactly this: two sites (local-allocation
    + `lower_assign_stmt` aggregate-dst branch) had
    out-of-sync inline patterns, so adding `Ty::Optional` to
    `is_aggregate_ty` alone didn't help — Optional locals
    silently routed into the wrong storage. After O.1 both
    sites go through the helper; future aggregate variants
    (sum types, tagged unions, …) auto-fix.

---

<a name="after-phase-1"></a>
## After Phase 1 — what's next

The architecture's Phase-1 exit gate (200-program corpus) is met
**and** the Phase-1 follow-up "Option A" (class/slice ABI + `as` casts)
landed across A.1–A.4. The "Option B" Phase-13 LLVM backend then
shipped across B.1–B.5. The "Option C" Phase-2 entry has now landed
three sub-bundles: c-strings (C.1 + C.2 + cleanup #1), match
(M.1 + M.2 + M.3), and the `?T` tracer (O.1). The remaining
Phase-2 sub-bundles are O.2 (`?T` match patterns + nil arm), O.3
(`!T` error union + `!`-assert), and comptime + modules.

### Option A — DONE

A.1–A.4 shipped in this order: `as` int↔int (c1b091e), `as` float
bridge (258cc70), class-by-pointer ABI (a6dc722), slice-by-pointer
ABI (5d71372). One bug yielded across the four sub-bundles (the
latent `def_to_fn` off-by-N surfaced by A.3). Corpus 200 → 226;
unit tests 121 → 147.

### Option B — DONE

B.1–B.5 shipped in this order: tracer bullet (0c3a9fe), int +
control flow + extern + recursion (9384331), float ops + `as`
matrix (9e6192c), aggregate ABI (1129232), string literals + Print
desugar (8c2a6df). Zero bugs yielded across the five sub-bundles —
the dual-backend invariant held bit-exactly across saturating fcvt,
ordered float comparisons, sign-aware integer ops, and the System V
"memory class" aggregate ABI. LLVM corpus 0 → 226 (full parity);
unit tests 147 → 148. The architecture's Part F.2 LLVM mandate is
now satisfied; Cranelift remains as the placeholder for the Phase 7
TPDE port.

### Option C — Phase 2: comptime + module system

The big jump. Phase 2 brings:
- `cipher` package manager (workspace's `arsenal_cipher` stub).
- `frequencies` module imports (`use std::mem::Foo`).
- `comptime` evaluator (workspace's `arsenal_comptime` stub).
- `match` expressions — **M.1 + M.2 + M.3 DONE** (commits
  `183e5b8` + `7d9c04d` + `2d85e65`). `match` accepts integer-
  literal, bool-literal, inclusive-range (`lo..=hi`), or-pattern
  (`a | b | c`), and wildcard forms; the MIR-side
  `lower_pattern_test` helper recurses for or-patterns and emits
  short-circuit pairs for ranges; codegen unchanged.
- error unions `!T`, optional types `?T` — **O.1 (?T tracer) DONE**
  (commit `7c46d5b`). `?T` as a 2-field aggregate {tag, payload}
  for primitive inners; `nil` literal; `T → ?T` coercion; `??`
  coalesce operator with lazy RHS. O.2 adds `match` patterns over
  `?T`; O.3 adds `!T` error union + `!`-assert.
- `c"..."` C-string literals — **C.1 + C.2 DONE** (commits
  `1e8752c` + `bd3cf5d`). `c"..."` types as `[*:0]u8`, lowers via
  `Const::CStrAddr` to a parallel `__gw_cstr_<i>` rodata pass in
  both backends, decays to `*u8` at extern call sites; non-extern
  fn signatures accept `[*:0]u8` directly.

Estimated cost remaining: dozens of hours. Bug yield so far is
1 across all six closed Phase-2 sub-bundles (C.1+C.2+M.1+M.2+M.3 = 0,
O.1 = 1, against a 12/A.x prediction of ~4). The remaining
sub-bundles introduce additional *value-level* novelty (tag-byte
reads in pattern matching for O.2; new aggregate layout + trap
machinery for O.3; comptime evaluator state and multi-file build
orchestration for the comptime sub-bundle), so the prediction
stays armed.

The dual-backend test now in place means any Phase 2 codegen change
is automatically validated against both Cranelift and LLVM; this
became useful immediately on C.1 and stayed useful through M.3.

#### Recommended next sub-bundle: O.2 (`?T` match patterns)

After O.1 closes the `?T` tracer, the natural next step is
extending the existing match infrastructure (M.1–M.3) to scrutinise
optional values. The remaining Phase-2 surface in ascending cost
order:

- **O.2 `?T` match patterns** — adds `match opt { nil => ..., _ =>
  ... }` (and possibly bare-pattern wildcards on the some-arm
  side, depending on whether O.2 includes binding patterns or
  defers them to O.4). MIR adds a `nil`-pattern arm to
  `lower_pattern_test` that emits `cmp = (tag == 0); Branch`.
  Typeck extends `check_match_pattern` to handle Optional
  scrutinees. Bug yield estimate: ~1 (new pattern + tag-byte read
  combination, but reuses both M.x and O.1 machinery).
- **O.3 `!T` error union + `!`-assert** — `!T` is a 2-field
  aggregate parallel to `?T` with an error-tag-and-payload
  layout. `expr!` asserts the success tag and traps on the error
  tag. Needs trap-on-mismatch machinery (a new `Terminator::Trap`
  or reuse of `Terminator::Unreachable` with a side-channel
  diagnostic). New value-level novelty re-arms the prediction —
  ~1-2 bugs.
- **comptime + frequencies (modules)** — the architectural heavy
  lift. New `arsenal_comptime` crate, multi-file builds, module
  resolution. Should land in a session that can be devoted entirely
  to it; gates `cipher` / `frequencies` so blocks the manifest-
  driven driver UX.

The argument for O.2 → O.3 next (in this order):
- O.2 reuses both M.x (decision-tree lowering) and O.1 (Optional
  aggregate layout) infrastructure — it's the smallest possible
  next step that actually exercises something new (tag-byte read
  in a pattern test).
- O.3 introduces a parallel aggregate layout (`!T`) that's
  structurally similar to `?T` but with different semantics
  (error vs nil); the lessons from O.1's bug surface (canonical
  `is_aggregate_ty` predicate) should carry forward and prevent a
  repeat.
- Both can ship within a single session.
- Neither depends on comptime / modules and neither gates them.

### Tactical cleanup (any session)

These are all self-contained and worth landing whenever a session
runs short on time for the bigger items:

1. ~~**Default `-> u0` for fn declarations.** One arm in
   `parse_fn_decl`.~~ **DONE in `e394571`** (cleanup #1) — the
   fix lived in typeck, not the parser; the parser already
   accepted the optional `RetType`. Fall-through-without-return
   from a `u0` fn is implicitly confirmed by corpus #229
   (`fn greet(s: [*:0]u8) { puts(s); }` runs cleanly on both
   backends), which subsumes what was the original cleanup #3.
2. **Fix `arsenal new` templates** so the generated `hello.gw` parses
   under Phase-1 syntax. The bare-string-literal half already works
   after 11c; the `#virtuous` directive is still rejected. Easiest
   fix: rewrite the templates to use today's syntax.
3. **Float `Mod` and `Pow`** codegen arms (Cranelift falls through
   to integer ops; LLVM returns `Unsupported`. Both are harmless
   today — typeck doesn't produce them — but neither is
   future-proof. Add float arms in both backends together so they
   stay in lockstep).
4. **Non-`u8` slice elements.** `resolve_type` for `Ty::Slice` only
   accepts `[]u8` today; A.4 didn't widen this. Both backends'
   aggregate paths already handle arbitrary 8-byte fields, so the
   typeck-only change is small. Worth ~30 min if a corpus program
   wants `[]i32`.
5. **`ld: warning: no platform load command found`** spam from
   LLVM-emitted Mach-O objects on macOS. The LLVM module isn't
   tagging the object with `LC_BUILD_VERSION`. Cosmetic; binaries
   still run. Likely fix: either set the macOS triple's deployment
   target on the `TargetMachine` or add an `-mmacosx-version-min`
   flag at the `cc` invocation. Trivial when someone gets annoyed
   enough.
6. **Restore Windows to the CI matrix.** Currently dropped because
   `llvm-sys 180` has no working install path on GitHub's
   `windows-latest` runners. Two practical paths: feature-gate
   `arsenal_codegen_llvm` so Windows can `cargo build --workspace`
   without it (the more honest fix; touches `arsenal_driver`'s
   backend-dispatch code in `cmd_build.rs`), or find an LLVM 18 dev
   distribution for Windows that ships `llvm-config.exe` + the
   static archives (vcpkg may work; chocolatey's `llvm` package
   does not). Either fix should re-add `windows-latest` to the
   `os` matrix in `ci.yml`.

---

## Working method (for next session)

We've been operating with a tight rhythm. Reproduce it:

1. **Read this doc + the latest commit message before any code.**
2. **Surface design decisions before writing code.** "Two open questions
   before I start" is the right opening move for any non-trivial increment.
3. **Tracer bullet within each increment**: get the *thinnest possible*
   end-to-end test passing first, then add corpus programs. New compiler
   code that isn't exercised by an end-to-end run-test isn't done.
4. **Regression tests for every bug.** When something miscompiles, write
   a unit test in the relevant crate that asserts the *fix*, not just
   that "it works now". Examples: `let_binding_with_temped_init_resolves
   _to_correct_local`, `else_if_chain_branches_into_nested_if`.
5. **Commit per increment, never per file change.** One commit per
   incremental capability.
6. **Workspace sweep before committing**:
   ```
   cargo build --workspace
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --all -- --check
   ```
   Fix all four to green. Then commit.
7. **Commit message structure**: header line ≤72 chars; first paragraph
   describes the increment; later paragraphs detail each notable
   change including any bug fix's mechanism. End with `Co-Authored-By`
   line. See `git log -10` for examples.
8. **`.expected_stdout` for any test program that prints**, even if the
   "exit code is enough" — early breakage is easier to catch when both
   are checked.

---

## Pre-flight checklist

Run these at the start of the next session to verify the tree is in the
state this doc describes:

```bash
cd /Users/silmaril/Documents/GitHub/gw
git log --oneline | head -10
# expect tip: HANDOFF refresh after O.1 (this commit), 7c46d5b (O.1
#             ?T tracer), 598cece (HANDOFF after match sub-bundle),
#             2d85e65 (M.3 range + or-patterns), 7d9c04d (M.2 bool +
#             stmt-position), 183e5b8 (M.1 match tracer), d8a217e
#             (HANDOFF after C.1/C.2 + cleanup #1), bd3cf5d (C.2
#             sentinel ptr), 1e8752c (C.1 c-string tracer), e394571
#             (cleanup #1 default -> u0) at the bottom of head -10.

git status
# expect: clean working tree (no .DS_Store, no .probe leftovers)

# LLVM 18 must be installed and discoverable for the workspace to build.
# On macOS:
which /opt/homebrew/opt/llvm@18/bin/llvm-config && /opt/homebrew/opt/llvm@18/bin/llvm-config --version
# expect: "18.x.x" (any 18.x — `brew install llvm@18` installs the
# current bottle, currently 18.1.8). If absent, `brew install llvm@18`
# and `brew install zstd` (LLVM 18's bottle links zstd).

export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18

. "$HOME/.cargo/env"
cargo test --manifest-path compiler/arsenal-boot/Cargo.toml --workspace 2>&1 | grep "test result" | awk '{p+=$4;f+=$6}END{print p,f}'
# expect: "185 0"

ls tests/snake_eater/pass/phase1/*.gw | wc -l
# expect: 239

ls compiler/arsenal-boot/crates/ | wc -l
# expect: 17
```

If any of those fail, **don't start the next session's work** —
investigate first. The most likely culprits are stale `target/`
directories, an outdated `Cargo.lock`, or someone else's commits
between sessions. If `cargo build` fails inside `arsenal_codegen_llvm`
or `llvm-sys`, double-check `LLVM_SYS_180_PREFIX` and make sure the
LLVM 18 install hasn't been replaced by a newer version (the pin is
to 18.x specifically; 19+ won't work without bumping the inkwell
feature flag in `[workspace.dependencies]`).

---

## Appendix — useful commands

```bash
# Build the compiler (needs LLVM_SYS_180_PREFIX in the env)
export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18
cargo build --manifest-path compiler/arsenal-boot/Cargo.toml -p arsenal_driver

# Compile and run a single .gw file (Cranelift, default)
ARSENAL=compiler/arsenal-boot/target/debug/arsenal
$ARSENAL build path/to/file.gw
./path/to/file
echo $?

# Same file through the LLVM backend
$ARSENAL build --backend=llvm path/to/file.gw
./path/to/file
echo $?

# Inspect the AST for a file (Phase 0 dump)
$ARSENAL dump path/to/file.gw

# Disassemble a compiled binary (helpful for codegen bugs)
otool -tv path/to/binary  # macOS
objdump -d path/to/binary # Linux

# Run only the Phase-1 run-corpus through Cranelift
cargo test --manifest-path compiler/arsenal-boot/Cargo.toml \
    -p arsenal_driver --test phase1_run

# Run the same corpus through the LLVM backend
cargo test --manifest-path compiler/arsenal-boot/Cargo.toml \
    -p arsenal_driver --test llvm_backend

# Run just the lex+parse snapshot corpus
cargo test --manifest-path compiler/arsenal-boot/Cargo.toml \
    -p arsenal_parse --test snake_eater

# Update insta snapshots after intentional changes
INSTA_UPDATE=always cargo test \
    --manifest-path compiler/arsenal-boot/Cargo.toml \
    -p arsenal_parse --test snake_eater

# Inspect generated LLVM IR for a single file (handy when debugging a
# B.x miscompile — emit .ll instead of .o by tweaking the driver, OR
# disassemble the .o to confirm what landed):
otool -tv path/to/file       # disassembled native code
```

---

## One-line architecture summary

```
                                              ┌─→ Cranelift ─┐
.gw → lex → parse → resolve → typeck → MIR ───┤              ├─→ object → cc → executable
                                              └─→ LLVM 18 ───┘
```

Each arrow is a separate active crate; each bug we've caught lived at exactly
one of those arrows. Increment 11 (a/b/c) extended the leftmost arrows
(parser accepts top-level stmts and `*T` raw pointers; `[]u8` slice type
with `.rodata` storage; implicit Print desugar at statement position).
Increment 12 didn't change the arrow topology either, but it forced four
small but real fixes that lived inside the existing arrows: `fcmp` dispatch
in codegen (12a), short-circuit control-flow lowering in MIR (12b),
bidirectional literal narrowing in typeck (12d/12h), and the regression
test suites that pin those fixes in place. The A.1–A.4 follow-up
extended every arrow except `lex` and `resolve`: parser added postfix
`as`, AST added `CastExpr`, typeck added `synth_cast` plus the dropped
class/slice rejections, MIR added `Rvalue::Cast` and the `def_to_fn`
fix, codegen added the seven `CastKind` arms and the aggregate-by-
pointer ABI. The B.1–B.5 bundle added the LLVM fork on the right side
of the diamond — same MIR consumed by both backends, same `Vec<u8>`
object-bytes shape produced. The C.1 + C.2 + cleanup #1 sub-bundle
extended every arrow except `lex` and `resolve` again: parser added
the `[*:S]T` arm (C.2), AST promoted `SentinelPtrType` from `Stub`
(C.2), typeck added `Ty::SentinelPtr` + the `[*:S]T → *T` coercion
+ the default `-> u0` (C.2 / cleanup #1), MIR added `Const::CStrAddr`
+ the parallel `cstring_literals` table (C.1), both backends added
the `__gw_cstr_<i>` rodata pass (C.1) and explicit `Ty::SentinelPtr`
arms (C.2). The match sub-bundle (M.1 + M.2 + M.3) extended the
same arrows again: parser added `parse_match_expr` /
`parse_match_pattern` / `parse_pattern_literal_value` and the
`RangePat` SyntaxKind (M.3); AST promoted `Expr::Match`,
`Pattern::Literal`, `Pattern::Range`, `Pattern::Or` from `Stub`;
typeck added `synth_match` + `check_match_pattern` with the
exhaustiveness rule; MIR added `lower_match` + the recursive
`lower_pattern_test` helper. Both backends still gain zero
new arms across the entire match sub-bundle (the chain-of-Branch
shape was already validated by 12b). The O.1 `?T` tracer extended
the same arrows yet again: parser added `??` (16, 15) right-assoc;
typeck added `Ty::Optional(OptInner)` with closed inner enum, plus
the `T → ?T` coercion rule and `nil`-narrowing; MIR added
`let_ty_from_ast`, `wrap_to_optional_if_needed`, `lower_nil_literal`,
and `lower_coalesce` (3-block tag-test CFG); both backends added
`Ty::Optional` arms to `is_aggregate_ty` / `aggregate_layout` /
`aggregate_field_ty` (via the shared `optional_layout` formula
so the by-pointer ABI agrees byte-for-byte). The 239-program
corpus is the direct test surface for every one of those arrows,
exercised through both backends in CI.
