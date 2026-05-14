# GW Bootstrap — Session Handoff

This document is the entry point for the next development session. Read it first.

> **Naming rename (2026-05-11):** the Metal Gear–themed naming layer
> (`arsenal`, `cipher`, `liberty`, `foxdie`, `snake_eater`, `virtuous`,
> `MotherBase`, …) has been removed in favour of plain English. The
> binary is now `gw` (was `arsenal`); the workspace is
> `compiler/gw-bootstrap/` (was `compiler/arsenal-boot/`); every crate
> is `gw_<role>` (was `arsenal_<role>`, special case
> `arsenal_cipher` → `gw_pkg`); the test corpus lives at
> `tests/corpus/` (was `tests/snake_eater/`); the live language
> keyword `liberty` is now `mod`. Ledger commit hashes below predate
> the rename — entries are kept verbatim because the renamed identifiers
> are mechanically derivable. See the original rename plan in the
> session transcript and commits 1–6 on `main`.

> **Last updated:** 2026-05-14, after closing CT.3b — comptime
> string literals (commit `f8bd7df`) on top of CT.3a
> (`0b3ccba`), block-like-tail (`9ac51a1`), implicit-tail-return
> (`579c4f0`), and CT.2's closure. `comptime { "hello" }`
> evaluates to a string value that materialises at runtime as
> the same `[]u8` slice aggregate that runtime string literals
> build; both backends agree byte-for-byte on the rodata payload.
> **Repo root:** `/Users/silmaril/Documents/GitHub/gw`
> **Workspace tests:** 292 unit + integration, all green.
> **Corpus:** 62 Phase-0 lex+parse snapshots + 248 Phase-1 + 31
> Phase-2 comptime single-file run-tests + 4 Phase-2 multi-file
> projects.

---

## TL;DR

The GW bootstrap compiler at `compiler/gw-bootstrap/` is a working
Rust implementation that takes `.gw` source through lex → parse →
resolve → typeck → MIR → codegen → link to a native executable.
Two backends ship in the same workspace: `gw build --backend=fast`
(Cranelift, default) and `gw build --backend=llvm` (LLVM 18 via
`inkwell`). Both consume the same MIR and agree bit-exactly across
the 248-program Phase-1 corpus + 4 multi-file projects + the 31
Phase-2 comptime tracers. Phase 0, Phase 1, Phase 13 (LLVM), and
the Phase-2 entry (c-strings, `match`, `?T`/`!T`, modules) are
closed. **Phase 2 / CT.2 is now entirely closed** across five
sub-bundles: CT.1 (comptime tracer — integer-literal blocks via
a typeck-side tree-walking interpreter), CT.2a (integer
arithmetic `+ - * / %` over `i128` with `IntegerOverflow` /
`DivisionByZero` error variants), CT.2b (comparisons + `CtValue
::Bool` — four ordering ops `< <= > >=` and overloaded equality
`== !=` for both int and bool operands, op-first dispatch
through `expect_int`), CT.2c (let-bindings + locals env — dense
`Vec<Option<CtValue>>` indexed by `BindingId.0`, decoupled from
`gw_typeck` via a `BindingEnv` trait, `NodePtr` moved to
`gw_ast::cst`), CT.2d (`if`/`else` branch-eval discipline — only
the taken arm evaluates), and CT.2e (short-circuit `&&` / `||`
— LHS first, RHS only when LHS doesn't determine the result).
**Implicit-tail-return for bare-expression tails** landed first:
`fn add(a, b) -> i32 { a + b }` and `fn answer() -> i32 { 42 }`
compile cleanly — typeck checks the tail against the declared
return type via the existing bidirectional narrowing, and MIR
wires the tail operand into `Terminator::Return`. **Block-like-
tail parser widening + divergent-tail discard** then extended
the shape: `parse_stmt`'s `KwIf | KwWhile | KwFor | LBrace` arm
now uses the same checkpoint trick as `parse_expr_stmt`, so
`fn classify(x: i32) -> i32 { if x < 0 { 1 } else { 2 } }` and
`fn dots(n: i32) -> u0 { while … }` work end-to-end. Typeck's
divergent-tail rule ("discard u0 tails when fn returns non-u0")
preserves the pre-existing semantics of `25_if_else.gw` /
`27_else_if.gw` / `163_print_padding.gw` — programs whose
block-like tail types as `u0` because all arms `return`. MIR
mirrors the discard via `expr_types` lookup. CT.2d's comptime
paren-wrap workaround is retired (one parser change, two
unblocked surfaces). **CT.3a — comptime float arithmetic +
comparisons** then opened the CT.3 family: `CtValue::Float(f64)`
joins `Int` and `Bool`; `eval_binary`'s arithmetic / ordering /
equality arms now dispatch on the operand tuple admitting both
`(Int, Int)` and `(Float, Float)` pairs; IEEE-754 semantics are
mirrored bit-for-bit (NaN == NaN is false, NaN ordering returns
false, division by 0.0 yields ±∞ without raising); MIR's
`lower_comptime` gains the `Const::Float` materialisation arm
with f32 / f64 width narrowing. **CT.3b — comptime string
literals** then added `CtValue::Str(Vec<u8>)` (owned-inline;
`CtValue` lost its `Copy` impl to make room for the
non-Copy payload). `comptime { "hello" }` decodes via a
`decode_string_literal` helper kept in lockstep with `gw_mir`'s
runtime decoder; MIR's `lower_comptime` materialises bytes as
the same `{data, len}` `[]u8` slice aggregate that runtime
string literals build, sharing the rodata interning path via
`lcx.string_literals`. No comptime operations on strings
(concat, `==`, `.len`) yet — they ride a future sub-bundle
motivated by corpus need. The remaining CT.3 surface is
classes, optionals, and error unions as the corpus
motivates. The decl-level `comptime fn foo() -> T { ... }` form
is *deferred to Phase 5* (where the evaluator becomes a stack VM
on MIR and `comptime fn` is a one-bit annotation rather than
synthesised AST-inlining); module-level
`let CONSTANT: T = comptime { ... };` covers the shared-constant
use case in Phase 2.

---

## Where to start the next session

Read this whole document, then in priority order:

1. **`docs/spec.md` §5.3** (lexical structure) — refresher only.
2. **`docs/architecture.md` Part L Phase 1 deliverables** — the contract.
3. **`docs/architecture.md` Part B.3, C.3, D.1, F.1** — pipeline shape.
4. **The most recent commit message** (`git log -1`) — picks up the thread.
5. **`tests/corpus/pass/phase1/`** — skim a few `.gw` files to see
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
├── tests/corpus/
│   ├── pass/lexparse/           (61 .gw + insta snapshots — Phase 0)
│   ├── pass/phase1/             (248 .gw + .expected_exit / .expected_stdout)
│   └── fail/lexparse/           (5 .gw + .expected_diagnostics)
├── compiler/gw-bootstrap/       (Cargo workspace — host = Rust 1.95+)
│   └── crates/
│       ├── gw_lex/         ★ active
│       ├── gw_ast/         ★ active
│       ├── gw_parse/       ★ active
│       ├── gw_resolve/     ★ active
│       ├── gw_typeck/      ★ active
│       ├── gw_mir/         ★ active
│       ├── gw_comptime/    ★ active (CT.1: typeck-side tree-walking interpreter)
│       ├── gw_codegen_fast/★ active (Cranelift-backed)
│       ├── gw_codegen_llvm/★ active (LLVM-18-backed via inkwell, Phase 13)
│       ├── gw_driver/      ★ active (the `gw` binary)
│       ├── gw_borrow/             stub  (Phase 3)
│       ├── gw_lir/                stub  (Phase 7)
│       ├── gw_jit/                stub  (Phase 7)
│       ├── gw_lsp/                stub  (Phase 9)
│       ├── gw_fmt/                stub  (Phase 9)
│       ├── gw_doc/                stub  (Phase 9)
│       └── gw_pkg/                stub  (Phase 9 — package manager)
└── .github/workflows/ci.yml      (Linux/macOS/Windows matrix)
```

### Active crate roles (≈8 100 LoC of compiler logic)

| Crate | Phase | Role |
|---|---|---|
| `gw_lex` | 0 | UTF-8 lexer state machine. 108-variant `TokenKind`, phf keyword table, `Span`/`SourceMap`/`Diagnostic`/`DiagBag` types. |
| `gw_ast` | 0 | Hand-rolled rowan-style CST + typed AST. Single unified `SyntaxKind` enum (189 variants — `RangePat` added in M.3). Typed views for ~38 Phase-1 / Phase-2 node kinds; `Stub` variants for the rest. `Module::stmts()` exposes top-level stmts (11a). `CastExpr` typed view added in A.1. **`SentinelPtrType` typed view (C.2)** with `element()` + `sentinel()` accessors. **`Expr::Match` (M.1)** + `MatchExpr::scrutinee()` / `arms()`, `MatchArmList::arms()`, `MatchArm::pattern()` / `body()`. **`Pattern::Literal` (M.1) / `Range` (M.3) / `Or` (M.3)** promoted from `Stub`; views expose `value()` / `lo()` + `hi()` / `alternatives()` respectively. **`Expr::Comptime(ComptimeExpr)` (CT.1)** promoted from `Stub` with a `block()` accessor returning the single inner `Block`. The pre-existing `Block::tail_expr()` accessor is now reachable for the first time — CT.1's parser change populates the bare-Expr tail slot it consults. **`NodePtr<'a>` (CT.2c)** moved here from `gw_typeck` so downstream crates (`gw_comptime`'s `BindingEnv` lookups, `gw_mir`'s `comptime_values` reads) can key into typeck's side-tables without depending on `gw_typeck` (which would form a dep cycle for `gw_comptime`). `gw_typeck` keeps a `pub use gw_ast::cst::NodePtr` so existing consumers' import paths still work. Bumpalo arena per file. Pretty-printer for `gw dump`. |
| `gw_parse` | 0 | Recursive-descent + Pratt expression precedence. Error-recovering. Produces both CST and AST. No parser generator. `parse_module` forks on `peek_item_keyword` between item and stmt (11a). `parse_type` handles `*T` / `[]T` / `&T` / `?T` / `[N]T` / **`[*:S]T` (C.2 — sentinel many-pointer; peek-at-1 of `Star` distinguishes from slice / array)**. **Postfix `as Type` (A.1)** at left binding power 22 — between `*`/`/`/`%` (19/20) and prefix unary (23), matching Rust precedence so `-1 as u32` parses as `(-1) as u32`. **Match (M.1–M.3)**: `parse_match_expr` invoked from `parse_primary` on `KwMatch`; scrutinee parsed with `struct_literals_allowed = false`. New `parse_match_pattern` separate from `parse_pattern` (used by `let` / `for in`) — match-arm patterns accept `_` / `Ident` / `IntLit` / `Minus IntLit` / `KwTrue` / `KwFalse` / `lo..=hi` / `a \| b \| c` chains; the literal-side parsing uses a custom `parse_pattern_literal_value` instead of `parse_expr` so `\|` (bp 9, bitwise OR) and `..=` stay available for the pattern grammar. Or-pattern wrapping uses `start_node_at` checkpoint; range-pattern wrapping uses the same trick. **Comptime (CT.1)**: new `parse_comptime_expr` invoked from `parse_atom` on `KwComptime`; consumes `comptime` + a single `Block`. **Tail-expression widening (CT.1 + block-like-tail)**: `parse_expr_stmt` uses a checkpoint — if the next token after the parsed expression is `}`, it leaves a bare `Expr` child rather than wrapping in `ExprStmt`. The block-like-tail sub-bundle (commit `9ac51a1`) extends the same trick to `parse_stmt`'s `LBrace | KwIf | KwWhile | KwFor` arm: at the enclosing `}` the block-like expression becomes the block's tail-expr; otherwise it still wraps in `ExprStmt`. Top-level statements close at `Eof` not `}`, so module-level behaviour is unchanged. One parser change unblocks both fn-body block-like tails and CT.2d's comptime paren-wrap workaround. |
| `gw_resolve` | 1 / 2 | Walks the AST, registers top-level fn + class defs, exports `primitive_type_name()`. `DefKind::SyntheticMain` is registered when top-level stmts coexist without explicit `fn main` (11a). **F.1 cross-file**: new `resolve_modules(primary, extras, ...)` accepts a primary module plus zero or more secondary modules; all defs go into one flat namespace by default. **F.2 modules**: each file's `mod <name>;` puts its items in `module_tables[name]` instead of the flat pool; `use foo;` imports those items. **F.3 per-file scoping**: `ResolvedModule` gains `file_scopes: FxHashMap<FileId, FxHashMap<String, DefId>>`; each file's effective scope = flat pool + own items + items from modules the file `use`s. New `lookup_in_file(file, name)` consults the per-file scope; backwards-compat `lookup(name)` falls back to flat for AST-test contexts. |
| `gw_typeck` | 1 / 2 | Bidirectional checker. `Ty` enum: `U0`/`Bool`/`Int(IntTy)`/`Float(FloatTy)`/`Rune`/`Class(DefId)`/`Slice(IntTy)`/`Ptr(IntTy)`/**`SentinelPtr { elem: IntTy, sentinel: u64 }` (C.2)**/**`Optional(OptInner)` (O.1)**/**`ErrorUnion(OptInner)` (O.3)** where `OptInner = Int(IntTy) \| Bool` is a closed enum/`Error`. Emits a `TypedModule` with per-CST-node `expr_types`, `path_bindings`, `pat_bindings`, `call_targets`, `sigs`, `classes`. Slice + raw-pointer surface (11b/11c) are FFI-restricted; sentinel-pointer surface (C.2) is *not* — `[*:0]u8` flows through non-extern fn signatures because the producer-side sentinel guarantee gives the safety raw `*T` lacks. **Bidirectional literal narrowing (12d/12h)**: `check_expr` calls `try_narrow_literal` first — bare `IntLit`/`FloatLit`, `Unary(Minus, Literal)`, and `Paren(...)` shapes adopt the expected width when the value fits; out-of-range diagnoses against the literal span. `synth_binop_operands` extends the same rule across binary operators so `n < 2` (with `n: i64`) types cleanly. **`synth_cast` (A.1/A.2)** accepts the full numeric matrix `(Int\|Float, Int\|Float)`; non-numeric pairs reject with `UNSUPPORTED_CONSTRUCT`. **Class-/slice-typed fn params and returns (A.3/A.4)** are accepted via the by-pointer ABI; the `UNSUPPORTED_CONSTRUCT` rejections in `check_fn_signature` were dropped. **C.1 / C.2**: `synth_literal` types `c"..."` as `Ty::SentinelPtr { U8, 0 }`; `ty_assignable` adds the lone coercion `[*:S]T → *T` so the C.1 tracer's `puts(c"hi")` shape works without an explicit cast; missing return type defaults to `Ty::U0` (cleanup #1) instead of diagnosing — error code 307 is retired. **Match (M.1–M.3)**: `synth_match` synthesises the scrutinee, validates each arm's pattern via `check_match_pattern`, unifies arm bodies (first non-Error arm sets the result type, subsequent arms are checked against it). `check_match_pattern` accepts wildcards everywhere, integer-typed literal patterns + integer ranges (`Range`) when scrutinee is `Ty::Int(_)` (re-using the bidirectional narrowing for both bounds), `true`/`false` patterns when scrutinee is `Ty::Bool`, and `Or` patterns by recursing on each alternative. Exhaustiveness rule: every `match` requires either a `_` arm or — for bool scrutinees — both `true` and `false` literal patterns at top-level arms. Identifier patterns and other shapes still diagnose with UNSUPPORTED_CONSTRUCT until later widenings. **Optional (O.1)**: `Type::Opt(inner)` resolves to `Ty::Optional(OptInner)` when inner is integer/bool primitive (other inners reject); `try_narrow_literal` recognises `nil` in any `?T` context and adopts the expected Optional; `synth_literal` for `nil` outside an Optional context now diagnoses TYPE_MISMATCH (used to fall through silently to `Ty::Error` and pass any check); `ty_assignable` adds the lone `T → ?T` coercion edge — value-level distinct (the wrap below) but uniform at the source. Reverse direction (`?T → T`) is rejected; the user must unwrap. `synth_binary` dispatches `??` to `synth_coalesce`: LHS must be Optional, RHS checks against the inner, result type is the inner. **Match-on-?T (O.2)**: `check_match_pattern`'s Literal arm gains an Optional-scrutinee branch — `nil` accepts (records `expr_types[value] = scrut_ty`), other literals reject with a "use `_` to match the some side" hint. New `is_nil_literal` helper recurses through parens. The exhaustiveness rule fires unchanged for Optional scrutinees because they're not bool. **Error union (O.3)**: `Type::ErrorUnion(inner)` resolves to `Ty::ErrorUnion(OptInner)` with the same primitive-only constraint as O.1. `ty_assignable` adds a `T → !T` coercion edge parallel to `T → ?T`; `?T` and `!T` stay type-distinct (no exchange in either direction). New `synth_must` for `expr!`: LHS must be `Ty::ErrorUnion(_)`, result is the unwrapped inner. **Per-file scoping (F.3)**: `Cx` gains a `current_file: FileId` field set by `check_fn_body` from the fn's syntax span and by `check_synthetic_main_body` from the module's. Three name-lookup sites (`synth_path`'s top-level fn check, `synth_struct_lit`'s class lookup, `synth_call`'s callee resolution) switch from `cx.tm.resolved.lookup(name)` to `cx.tm.resolved.lookup_in_file(cx.current_file, name)`; `resolve_type`'s class-lookup site reads the file from the path's syntax span directly. **Comptime (CT.1 + CT.2a + CT.2b + CT.2c + CT.2d + CT.2e + CT.3a + CT.3b)**: new `synth_comptime` synthesises the inner block's type (so subexpressions populate `expr_types`), then runs `gw_comptime::eval_comptime_block`. CT.2d's `if`/`else` flows through the existing `synth_if` path — both arms synthesise to confirm their types match — without any CT.2d-specific typeck change; the divergence between typeck's walk-both-arms shape and the evaluator's walk-one-arm shape is contained inside `gw_comptime::eval_if`. CT.2e similarly adds no typeck-side code — `synth_binary`'s `AmpAmp` / `PipePipe` arm already eagerly synthesises both operands (both must be `Ty::Bool`), and the runtime / comptime divergence ("evaluate both" vs "short-circuit") is contained inside `gw_comptime::eval_logical_short_circuit`. On success, the resulting `CtValue` is stashed in new `TypedModule::comptime_values: FxHashMap<NodePtr, CtValue>`; on failure, new error code E0314 `COMPTIME_EVAL_FAILED` is pushed at the offending span. CT.1 realises integer-valued comptime blocks only; **CT.2b** widens the inner-type gate to `Ty::Int(_) | Ty::Bool` so the new ordering / equality comparisons (which produce `Ty::Bool`) flow through the same materialisation path. **CT.3a** widens the gate again to `Ty::Int(_) | Ty::Bool | Ty::Float(_)` so float-valued blocks like `comptime { 3.14 + 0.5 }` flow through. **CT.3b** widens the gate one more time to `Ty::Int(_) | Ty::Bool | Ty::Float(_) | Ty::Slice(IntTy::U8)` so string-literal blocks like `comptime { "hello" }` flow through. Wider inners (classes, optionals) still reject with `UNSUPPORTED_CONSTRUCT`, naming the supported set ("`int`, `bool`, `float`, and `[]u8` string blocks only"). **CT.3a** also adds the `EvalError::BadFloatLiteral` arm in `comptime_error_message` ("comptime evaluation could not parse this float literal") routed through the existing E0314 diagnostic. **CT.2a** adds two error-message arms in `comptime_error_message` for the new `EvalError::IntegerOverflow` and `EvalError::DivisionByZero` variants — user-facing strings "comptime arithmetic overflowed `i128` during evaluation" and "comptime evaluation attempted division or modulo by zero" route through the same E0314 diagnostic. **CT.2c** adds a `TypeckBindingEnv<'a, 'tm>` adapter that borrows the typed module's `pat_bindings` / `path_bindings` maps and exposes them through `gw_comptime`'s `BindingEnv` trait, converting `BindingId` to its inner `u32` at the trait boundary. The adapter borrow is scoped so it releases before `synth_comptime` mutates `comptime_values`. The crate also gains a `pub use gw_ast::cst::NodePtr` re-export (NodePtr moved to `gw_ast::cst` in CT.2c — see the `gw_ast` row); existing `use gw_typeck::NodePtr` consumers (like `gw_mir`) see no API change. **Implicit-tail-return (replaces the CT.1 E0315 guard rail) + block-like-tail discard**: `check_fn_body`'s post-`check_block` arm now synthesises the tail's type when `body.tail_expr()` is set; if `tail_ty == Ty::U0` and `sig.ret` is non-u0, the tail is discarded (treated as if it were an `ExprStmt`) to preserve the `25_if_else.gw` / `27_else_if.gw` / `163_print_padding.gw` shapes whose if-expression types as u0 because all arms `return`. Otherwise it calls `check_expr(tail, sig.ret, &mut cx)` — the bidirectional narrowing already running for `let` initialisers and `return` operands (decision #16) handles literal-width adoption (`fn f() -> i64 { 42 }`) without new typeck machinery. A non-matching tail type (e.g. `fn f() -> u0 { 42 }`) diagnoses as the ordinary `TYPE_MISMATCH` (E0300). The CT.1-era E0315 `TAIL_EXPR_IN_FN_BODY` constant is retired (comment-only in `ec`) — the regular type-mismatch path carries the user-facing fix without special-casing. The bare-Expr surface landed first (commit `579c4f0`); the block-like-tail extension (commit `9ac51a1`) covers `KwIf` / `KwWhile` / `KwFor` / `LBrace` tails via the parser widening (see `gw_parse` row), and the divergent-tail discard rule ships in the same sub-bundle. CT.2d's comptime paren-wrap workaround is retired as a second consequence of the parser change. |
| `gw_mir` | 1 / 2 | CFG of basic blocks; primitive locals + aggregate stack-slot locals (class + slice); `Assign`/`AssignField` statements; `Use`/`BinOp`/`UnOp`/`Field`/`Cast` rvalues; `Goto`/`Branch`/`Return`/`Call`/`Unreachable` terminators. Loop-target stack for break/continue. `lower_for` desugar. `Const::DataAddr` + program-level `string_literals` table for `.rodata` payloads (11b). Implicit Print at stmt-position string lits desugars to `write(1, slice.data, slice.len)`; auto-injects `extern fn write` if user didn't declare one (11c). **Short-circuit `&&` / `\|\|` (12b)**: `lower_short_circuit` emits a 3-block control-flow shape (rhs-eval / short-circuit / join) and bypasses `lower_binary` so the RHS is only evaluated when the LHS doesn't determine the result. **`Rvalue::Cast` (A.1/A.2)** carries `kind: CastKind`, `operand`, `src_ty`, `dst_ty`; the closed `CastKind` enum has 7 variants, each maps to one Cranelift op. `select_cast_kind` factors the kind selection out of `lower_cast`. **`def_to_fn` fix (A.3)**: pre-A.3 the map stored each def's position in `resolved.defs` (including class defs); A.3 only counts `Fn`/`SyntheticMain` defs when assigning indices, matching the order `functions` is populated. **C.1 / C.2**: `Const::CStrAddr(CStrLitId)` + program-level `cstring_literals` table parallel to `string_literals` (no shared dedup keys — slice payloads and c-string payloads carry different semantics). `lower_cstring_literal` interns the decoded bytes (no NUL terminator stored — codegen appends it) and returns the operand directly without materialising a slice aggregate. **Match (M.1–M.3)**: `lower_match` allocates `body_bb` + `next_bb` per arm, calls the recursive `lower_pattern_test` helper, lowers the body in `body_bb`, restores cursor to `next_bb` for the next arm. `lower_pattern_test` emits `Goto(body_bb)` for wildcards, `cmp = Eq; Branch` for literals, two short-circuit `Ge` / `Le` tests for inclusive ranges, and recursive chains (each alternative threads through a fresh `alt_next_bb`) for or-patterns. The chain-of-Branch shape is the same control flow already used by short-circuit `&&` / `\|\|`, so codegen needs zero new arms across the entire match sub-bundle. **Optional (O.1)**: new `let_ty_from_ast` helper resolves `?T` annotations so `lower_let` allocates the binding local at the correct Optional aggregate type. `wrap_to_optional_if_needed` materialises the implicit `T → ?T` coercion at let-init time — allocates a fresh aggregate temp, writes tag = 1 + payload via `AssignField`, returns `Operand::Local`. `lower_nil_literal` mirrors the shape for `nil`: tag = 0, no payload write (the tag distinguishes empty). `lower_coalesce` emits the 3-block decision: read tag → compare tag == 0 → `Branch` into nil-default-block (lazy RHS evaluation, assign result) or some-payload-block (read field 1 directly into result). Both arms `Goto` a shared join. **Match-on-?T (O.2)**: `lower_pattern_test`'s Literal arm gains an Optional-scrutinee branch — read tag (`Rvalue::Field` with `field_idx = 0`), compare `tag == 0`, `Branch`. New helpers `pattern_value_is_nil` (recognise nil-literal patterns) and `ensure_scrut_local` (materialise an aggregate temp if `scrut_op` isn't already a `Local`). **Error union (O.3)**: `let_ty_from_ast` also resolves `!T` annotations. `wrap_to_optional_if_needed` generalised to `Ty::Optional(_) \| Ty::ErrorUnion(_)` — both share the `{tag, payload}` layout, so the wrap shape is identical. New `lower_must` for `expr!`: read tag, branch on `tag == 0` into a trap block (`Terminator::Unreachable`, which both backends lower as a hardware trap), read payload field on success. **The wrap helper now fires at three sites** — `lower_let`, `lower_return` (uses new `LowerCx::fn_return_ty`), and `lower_call` (consults `typed.sigs` for each callee param). **Comptime (CT.1 + CT.2b + CT.3a + CT.3b)**: new `lower_comptime(b, c, lcx)` pulls the pre-evaluated `CtValue` from `typed.comptime_values` (keyed by NodePtr) and either emits `Operand::Const(_)` directly or — for `Str` — builds a slice aggregate. The comptime block's body is *never* lowered — MIR sees only the materialised value. Four arms today: `(CtValue::Int(n), Ty::Int(int_ty)) → Const::Int { value: n, ty: int_ty }` (CT.1); `(CtValue::Bool(b), Ty::Bool) → Const::Bool(b)` (CT.2b); `(CtValue::Float(f), Ty::Float(float_ty)) → Const::Float { bits, ty: float_ty }` (CT.3a — width-aware bit pattern: `f.to_bits()` for `F64`, `(f as f32).to_bits() as u64` for `F32`, mirroring `lower_literal`'s runtime `FloatLit` path); and `(CtValue::Str(bytes), Ty::Slice(IntTy::U8)) → Operand::Local(dst)` (CT.3b — interns bytes into `lcx.string_literals`, allocates a fresh `Ty::Slice(IntTy::U8)` local, `AssignField data = Const::DataAddr(id)` + `AssignField len = Const::Int(n, USize)`, mirroring `lower_string_literal`'s runtime aggregate-build). `lower_comptime`'s signature gained `b: &mut Builder` in CT.3b so the aggregate arm could push statements; the integer / bool / float arms still emit a single `Operand::Const` and don't use the builder. `comptime_values.get(...)` shifted from `.copied()` to `.cloned()` in CT.3b because `CtValue` lost `Copy`. The catch-all `_` fires when typeck rejected the block (no stash) or when value-vs-type pairing is inconsistent; both fall back to `Const::Error`. **Implicit-tail-return + block-like-tail discard**: `lower_fn` captures `lower_block`'s returned operand; when `body.tail_expr().is_some()` and the trailing block has no terminator, sets `Terminator::Return(tail_operand)`. The block-like-tail sub-bundle (commit `9ac51a1`) adds a divergent-tail discard step that mirrors typeck's rule — reads the tail expression's type from `typed.expr_types[NodePtr(tail)]`; if `Ty::U0` while `sig.ret` is non-u0 (and not `Ty::Error`), drops the lowered operand instead of installing it as `Return`. The existing fall-through (`Return(Unit)` for u0/Error returns; `Unreachable` for non-u0 with no tail and no explicit `return`) then governs, which is correct for divergent tails where the if's join is unreachable in the CFG. Literal width flows automatically: `lower_literal` reads `typed.expr_types[NodePtr(lit)]` for the `IntTy`, and typeck's `check_expr(tail, sig.ret, …)` populates that entry with sig.ret. |
| `gw_comptime` | 2 | **CT.1 + CT.2a + CT.2b + CT.2c + CT.2d + CT.2e + CT.3a + CT.3b** (CT.2 closed; CT.3 underway): tree-walking interpreter on the typed AST. Public surface: `CtValue::{Int(i128), Bool(bool), Float(f64), Str(Vec<u8>)}` (`Clone + Debug`; lost `Copy` in CT.3b so `Vec<u8>` payloads work inline; wider variants ride CT.3c+), `BindingEnv<'a>` trait (`lookup_pat` / `lookup_path` returning `Option<u32>`), `NoBindings` zero-sized resolver for shapes with no let / path-to-local references, `EvalCx<'sm, 'env, 'a>` / `Budget` (architecture E.3 caps: 10⁹ steps, 1024 depth), `EvalError::{Unsupported{span, what}, BudgetExceeded, StackOverflow, BadIntLiteral, BadFloatLiteral(Span), IntegerOverflow(Span), DivisionByZero(Span)}`, `eval_comptime_block(Block, &mut EvalCx) -> Result<CtValue, EvalError>`. CT.1 accepts integer-valued blocks with zero statements and a tail expression of shape `IntLit / Paren(expr) / Unary(Minus, expr) / Block(of-same)`. **CT.2a** extends the tail shape to include `Binary(lhs, op, rhs)` for `op ∈ {Plus, Minus, Star, Slash, Percent}` over `CtValue::Int` operands. Arithmetic flows through `i128::checked_{add,sub,mul,div,rem}` — overflow raises `IntegerOverflow(span)`; `Slash`/`Percent` short-circuit on `rhs == 0` to `DivisionByZero(span)` so the two failure modes never confuse diagnostically. **CT.2b** adds the `CtValue::Bool(bool)` arm; `eval_literal` recognises `KwTrue` / `KwFalse`; `eval_binary` reorganises around op-first dispatch with three groups: arithmetic (CT.2a), integer ordering (`Lt, LtEq, Gt, GtEq`), and equality (`EqEq, BangEq`, overloaded for both `(Int, Int)` and `(Bool, Bool)` operand pairs — mixed pairs reject explicitly rather than inventing a dominant-type rule). New `expect_int(v, span)` canonical operand-type helper routes arithmetic and ordering ops through a single rejection site; `Unary(Minus, …)` also goes through it. **CT.2c** widens `EvalCx` to carry a `&dyn BindingEnv<'a>` resolver and a dense `Vec<Option<CtValue>>` locals env indexed by `BindingId.0 as usize` (decision Q5 ⇒ option (a) — see decision #50). `eval_comptime_block_inner` walks block statements: `Stmt::Let(l)` evaluates the init, looks up the pattern's binding index via the resolver, and stores the value at that index via `store_local`. New `Expr::Path(p)` arm in `eval_expr` reads `load_local(idx)` for the resolved binding index. **CT.2d** adds an `Expr::If(i)` arm in `eval_expr` calling a new `eval_if` function: condition is evaluated and pinned to `CtValue::Bool` via the new `expect_bool(v, span)` helper (parallels `expect_int` from decision #49), then exactly one arm runs based on the boolean — the un-taken arm is never visited. `else if` chains fall out naturally because `IfExpr::else_branch` returns an `Expr` (either `Block` for terminal `else` or `IfExpr` for chained `else if`), both already dispatched by `eval_expr`. **CT.2e** adds lazy short-circuit `&&` / `||`: `eval_binary` intercepts `SyntaxKind::AmpAmp` / `PipePipe` *before* its eager RHS eval and routes to a new `eval_logical_short_circuit(op, lhs, rhs, span, cx)` helper. The helper evaluates the LHS, pins to bool via `expect_bool`, short-circuits if `l == short_circuit_value` where `short_circuit_value = matches!(op, SyntaxKind::PipePipe)` (the operator's identity element under boolean conjunction / disjunction — `false` for `&&`, `true` for `||`). Otherwise evaluates the RHS and returns its bool value. Mirrors decision #15's runtime 3-block CFG (`lower_short_circuit`) compressed into one Rust function. Bool ordering (`true < false`) still rejects with `Unsupported`. **CT.3a** adds `CtValue::Float(f64)` (canonical at f64; MIR narrows to f32 at materialisation if the surrounding `Ty::Float(F32)`); `eval_literal` recognises `SyntaxKind::FloatLit` via a new `parse_float_literal` helper (mirrors `gw_mir`'s `raw.replace('_', "").parse::<f64>()`); `EvalError::BadFloatLiteral(Span)` joins `BadIntLiteral` for parse failures. `eval_binary`'s arithmetic / ordering / equality arms refactored to dispatch on the `(lv, rv)` operand tuple — admitting both `(Int, Int)` and `(Float, Float)` pairs and rejecting mixed/Bool-in-arithmetic combinations explicitly via the catch-all `_`. Float arithmetic uses Rust's IEEE-754 ops directly (`+ - * / %` are total; `/` and `%` by `0.0` yield `±∞` / `NaN` with **no `DivisionByZero` error** — distinct from the integer path which still raises); ordering / equality use Rust's `<` / `<=` / `>` / `>=` / `==` which already implement the IEEE-754 partial-order contract (any comparison involving `NaN` returns `false`, including `NaN == NaN`). `Expr::Unary(Minus)` dispatches on operand: `Int` via `wrapping_neg`, `Float` via `-f`, `Bool` / `Str` reject. The CT.2b-era `expect_int` helper is **removed** — its sole consumers (the int-only binary arms) went away in the refactor. `expect_bool` stays (consumed by `if`-condition + short-circuit `&&` / `||`); it rejects `Int` / `Float` / `Str` operands explicitly. **CT.3b** adds `CtValue::Str(Vec<u8>)` for decoded string-literal bytes (owned-inline; required dropping `Copy` from `CtValue`); `eval_literal` recognises `SyntaxKind::StringLit` and decodes via a new `decode_string_literal` helper kept byte-for-byte in lockstep with `gw_mir::decode_string_literal` (supported escapes: `\n \t \r \0 \\ \" \'`; unknown escapes pass through as backslash + byte). `RawStringLit` deliberately not handled — the runtime decoder has a latent mis-decode for `\\…\\` tokens that no corpus exercises; CT.3b stays in lockstep with the validated runtime path. No comptime operations on strings — the tuple-dispatch in `eval_binary` rejects `Str` operands via its existing catch-all `_`, pinned by the `arithmetic_on_string_rejects` unit test. Materialisation-time narrowing (when the `i128` result doesn't fit the surrounding runtime `IntTy`, or when an f64 narrows to f32) is a separate concern handled in `gw_typeck` / MIR. `parse_int_literal`, `parse_float_literal`, and `decode_string_literal` mirror `gw_mir`'s decoders so source-form decoding stays in lockstep across the two consumers. The crate depends only on `gw_ast` + `gw_lex`; per architecture Part B.11 / E.1 the Phase-5 replacement (stack VM on MIR) keeps the same on-disk semantics (`CtValue`, sandbox budgets, error variants). |
| `gw_codegen_fast` | 1 / 2 | Cranelift-backed (placeholder until Phase 7 TPDE port). Aggregate (class + slice) layouts → stack slots; field reads/writes → stack_load/stack_store; aggregate-aggregate assigns → field-by-field copy. String literals materialised via `module.declare_data` + `define_data_object` under `__gw_str_<i>` symbols (11b). `*T` raw pointers lower as pointer-sized scalars (11c). **Float comparisons (12a)**: `lower_binop` branches on `ty.is_float()` for `Eq`/`Ne`/`Lt`/`Le`/`Gt`/`Ge` — floats use `fcmp` with the matching `FloatCC`, ints keep `icmp`. **Cast lowering (A.1/A.2)**: `Rvalue::Cast` arm reads operand at `clif_ty(src_ty)` and applies one Cranelift op per `CastKind` — `sextend`/`uextend`/`ireduce` for ints, `fcvt_from_sint`/`fcvt_from_uint` and saturating `fcvt_to_*_sat` for int↔float, `fpromote`/`fdemote` for floats. Same-width `*Bitcast` variants need no instruction. **Aggregate-by-pointer ABI (A.3/A.4)**: `make_signature` prepends a hidden out-pointer for aggregate returns and substitutes pointer-typed `AbiParam` for aggregate params. `define_fn` defers the entry-block switch until the lower-block loop's first iteration to keep Cranelift's "fill before switching" rule satisfied; aggregate params copy in via `copy_aggregate_from_ptr`, and `Terminator::Return` for an aggregate-returning fn copies out through `copy_aggregate_to_ptr`. `Terminator::Call` prepends `stack_addr(dst_slot)` for aggregate returns and substitutes `stack_addr` for aggregate args. **C.1**: parallel `__gw_cstr_<i>` rodata pass — payload is `bytes ++ "\0"`; `Const::CStrAddr` lowers via `module.declare_data_in_func` + `ins.global_value` exactly like `Const::DataAddr`. **C.2**: explicit `Ty::SentinelPtr { .. }` arms in `clif_ty` / `primitive_size_align` route to pointer-width — same shape as `Ty::Ptr`. **O.1 / O.3**: `is_aggregate_ty` extended to include `Ty::Optional(_)` and `Ty::ErrorUnion(_)`; `aggregate_layout` / `aggregate_field_ty` route both through the shared `optional_layout` formula (tag at offset 0 / 1 byte; payload at the inner's natural alignment; total size aligned to inner align). Local-allocation site + `lower_assign_stmt`'s aggregate-dst branch now both go through `is_aggregate_ty` — fixed two inline `matches!(..., Class \| Slice)` patterns from O.1 that silently routed Optional locals into the wrong storage and the wrong assign path (caught at the first dual-backend run). |
| `gw_codegen_llvm` | 13 / 2 | LLVM-18-backed via `inkwell` (B.1–B.5). Same `MirProgram → object bytes` contract as `gw_codegen_fast` — driver picks at `--backend=fast\|llvm`. Storage: alloca-per-local in the entry block (clang `-O0` style), `[N x i8]` allocas for aggregates with alignment bumped to the layout's max-field align via `InstructionValue::set_alignment`. Field addressing via byte-offset GEP through `i8` (opaque pointers; no struct types declared to LLVM). Bool stays at LLVM `i1` end-to-end (no i8 storage adapter). Float comparisons use ordered predicates (`OEQ`/`OLT`/etc.); float-→int casts route through the saturating `llvm.fpto{si,ui}.sat` intrinsics for Rust ≥ 1.45 / Cranelift parity. `Const::Float` lowers via `build_bit_cast(int_const, float_ty)` to preserve NaN payloads (a `const_float(f64)` round-trip would lose them on the F32 path). String literals materialise as one private `__gw_str_<i>` global per `MirProgram::string_literals` entry; `Const::DataAddr(id)` returns the global's address as `ptr`. Aggregate ABI: hidden out-pointer for aggregate returns; by-pointer for aggregate user params. `sret`/`byval` attributes intentionally omitted — corpus aggregates flow only between GW fns, plain-`ptr` agrees with Cranelift's manual `stack_addr` convention end-to-end. A small `build.rs` adds Homebrew's `lib` prefix to the linker search path on macOS so LLVM-18's system-libs (zstd, ffi, xml2, curses) resolve without `RUSTFLAGS` rituals. **C.1**: parallel pass for c-string globals — one private `__gw_cstr_<i>` per `MirProgram::cstring_literals` entry, payload `bytes ++ "\0"`; `Const::CStrAddr` returns the global's `as_pointer_value()`. **C.2**: explicit `Ty::SentinelPtr { .. }` arm in `llvm_basic_type` routes to opaque `ptr` — agrees with Cranelift's bit-exact output across all three c-string corpus programs. **O.1 / O.3**: `is_aggregate_ty` / `aggregate_layout` / `aggregate_field_ty` extended for `Ty::Optional(_)` and `Ty::ErrorUnion(_)` — same formula via the shared `optional_layout` helper, so the by-pointer ABI agrees byte-for-byte across backends. **O.3 also fixed `make_fn_type`**: the aggregate-return arm previously had a hardcoded allow-list (`Class \| Slice`) that excluded the new variants; now routes through `is_aggregate_ty` so future aggregate variants auto-handle. |
| `gw_driver` | 0 / 1 / 2 | Subcommands: `gw new <name>`, `gw build [--backend=fast\|llvm] <file.gw>`, `gw dump <path>`, `gw --version`. Build pipeline: lex → parse → resolve → typeck → MIR → (Cranelift OR LLVM) → object → `cc` link → executable. `--backend=fast` is the default; both backends emit the same `Vec<u8>` object-bytes shape so the linker invocation is shared. **F.1 multi-file builds**: the driver auto-discovers every other `.gw` file in the build target's parent directory, sorts by path (deterministic def order), reads each into one shared `SourceMap`, parses each into a `SyntaxNode<'bump>` (one `FileArena` per file, all sharing one `Bump`), folds parse diagnostics into the build's primary bag via `DiagBag::merge`, and passes the parsed roots to `resolve_modules`. The output executable still uses the build target's stem; sibling files contribute symbols but don't influence the output name. |

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
| B.1 | LLVM tracer bullet (`return 0` only) | `0c3a9fe` | (LLVM corpus 0 → 1) | `gw_codegen_llvm` from doc-comment stub to working `MirProgram → object bytes` via `inkwell`; `--backend=fast\|llvm` flag; `gw_codegen_llvm/build.rs` adds Homebrew lib paths on macOS for LLVM-18's system-libs (zstd / ffi / xml2 / curses); `gw_driver/tests/llvm_backend.rs` integration test; +1 integration test | 0 |
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
| O.2 | `?T` match patterns + `nil` arm | `c555777` | +1 (240) | parser `parse_pattern_literal_value` accepts `KwNil`; typeck `check_match_pattern`'s Literal arm gains Optional-scrutinee branch — `nil` accepts (records `expr_types[value] = scrut_ty`), other literals reject with hint to use `_` for the some side; new `is_nil_literal` helper recurses through parens; MIR `lower_pattern_test` Literal arm reads tag (`Rvalue::Field` field_idx 0), compares `tag == 0`, branches; new `pattern_value_is_nil` + `ensure_scrut_local` helpers; codegen unchanged (chain-of-Branch + Field reuse from M.x and O.1); +4 typeck unit tests | 0 |
| O.3 | `!T` error union + `!`-assert | `5282bc8` | +1 (241) | parser `!T` arm in `parse_type` produces `ErrorUnionType`; postfix `!` arm in `parse_expr_bp` produces `MustExpr`; AST `Type::ErrorUnion(ErrorUnionType)` and `Expr::Must(MustExpr)` promoted from `Stub`; new `Ty::ErrorUnion(OptInner)` variant reusing the same closed inner; `resolve_type` for `Type::ErrorUnion` accepts integer / bool inners; `ty_assignable` adds `T → !T` coercion (parallel to O.1's `T → ?T`); `?T` and `!T` stay type-distinct (no exchange). New `synth_must` (LHS must be ErrorUnion, result is inner). MIR `lower_must` emits 3-block decision (read tag → compare = 0 → Branch trap vs payload-read; trap-block uses `Terminator::Unreachable` which both backends lower as hardware trap); `wrap_to_optional_if_needed` generalised to `Optional \| ErrorUnion`. New `LowerCx::fn_return_ty` field carries the current fn's declared return type so `lower_return` applies the wrap; `lower_call` reads each callee param's type from `typed.sigs` and applies the wrap per-arg. Both backends extended for `Ty::ErrorUnion(_)` via the same `optional_layout` formula. **LLVM `make_fn_type` now routes the aggregate-return arm through `is_aggregate_ty`** instead of the inline `Class \| Slice` allow-list. +7 typeck unit tests | **2** — both fixed inline: (a) LLVM `make_fn_type` had a hardcoded `Class \| Slice` allow-list for aggregate returns, rejecting `!T` (and would have rejected `?T` if the O.1 tracer had used it). Fixed by routing through `is_aggregate_ty`. (b) MIR `lower_return` didn't apply the `T → ?T` / `T → !T` wrap when returning a bare `T` from an aggregate-typed fn. Cranelift produced exit 2 (low byte of un-wrapped primitive in aggregate slot); LLVM produced 7 (similar miscompile). Fixed by adding `LowerCx::fn_return_ty` and calling `wrap_to_optional_if_needed` from `lower_return`. Pre-emptively also fixed `lower_call`'s arg path (same shape, same risk). |
| F.1 | multi-file tracer (cross-file resolve, flat namespace) | `57b275d` | +2 multi-file projects (01, 02) | new `DiagBag::merge` drains another bag's diagnostics into self; new `resolve_modules(primary, extras, ...)` accepts a primary module plus zero or more secondary modules, all defs in one flat namespace; driver auto-discovers sibling `.gw` files in the build target's directory, sorts by path, reads each into the shared SourceMap, parses each into a `SyntaxNode<'bump>` (one `FileArena` per file, all sharing one `Bump`); top-level statements in sibling files diagnose with new TOP_LEVEL_STMTS_IN_LIBRARY (E0203); +3 resolver unit tests + 2 corpus projects (`01_add_two_files`, `02_cross_file_class`) | 0 |
| F.2 | `mod` + `use` declarations (opt-in modules) | `6969f64` | +1 multi-file project (03) | parser `parse_mod_decl` and `parse_use_decl`; AST `Item::Mod(ModDecl)` and `Item::Use(UseDecl)` promoted from `Stub` with `name()` accessors; resolver `process_module` puts items from `mod foo;` files in `module_tables[foo]` instead of the global flat `by_name`; F.2 globally flattens use'd module items into `by_name` (later refined in F.3); two new error codes E0204 UNKNOWN_MODULE and E0205 DUPLICATE_MOD; renamed fail fixture `f02_unsupported_mod.gw` → `f02_malformed_mod.gw` with refreshed expected diagnostics; +5 resolver unit tests + 1 corpus project (`03_mod_use`) | 0 |
| F.3 | per-file `use` scoping | `aab3f0b` | +1 multi-file project (04) | `ResolvedModule` gains `file_scopes: FxHashMap<FileId, FxHashMap<String, DefId>>`; new `lookup_in_file(file, name)` consults the per-file scope, falling back to flat `by_name` for AST-test callers without a file context; resolver post-pass builds each file's effective scope = flat pool + own items + items from modules the file `use`s; conflicts within a single file's scope diagnose as DUPLICATE_DEFINITION; F.2's global-import code path is gone — `by_name` is no longer enriched by `use` decls; `register_fn` / `register_class` return `(name, DefId)` so `process_module` can record file-local items; typeck `Cx` gains `current_file: FileId` field set by `check_fn_body` and `check_synthetic_main_body`; four name-lookup sites switch from `cx.tm.resolved.lookup` to `lookup_in_file(cx.current_file, name)`; +1 resolver unit test (`use_only_visible_in_declaring_file`) + 1 corpus project (`04_use_per_file`) | 0 |
| impl-tail-ret | implicit-tail-return for bare-expression tails (drops E0315; typeck checks tail against declared return type; MIR wires tail operand into `Terminator::Return`) | `579c4f0` | +4 phase1 fixtures (`200_tail_return_arith.gw` → 7, `201_tail_return_literal.gw` → 42, `202_tail_return_let_then_path.gw` → 22, `203_tail_return_widening.gw` → 100) | `gw_typeck` `check_fn_body` replaces the E0315 emission with `check_expr(tail, sig.ret, &mut cx)` — the bidirectional narrowing already running for `let` initialisers and `return` operands (decision #16) handles literal-width adoption (`fn f() -> i64 { 42 }`) without new typeck machinery. A non-matching tail type (e.g. `fn f() -> u0 { 42 }`) diagnoses as the ordinary `TYPE_MISMATCH` (E0300). The CT.1-era `ec::TAIL_EXPR_IN_FN_BODY` (E0315) constant is retired (replaced by a comment-only marker in `ec`). `gw_mir` `lower_fn` captures `lower_block`'s returned operand; if `body.tail_expr().is_some()` and the trailing block has no terminator, sets `Terminator::Return(tail_operand)`. The existing fall-through cases (u0/Error → `Return(Unit)`; non-u0 with no tail → `Unreachable`) stay unchanged for blocks without a tail expression. Literal width flows automatically: `lower_literal` reads `typed.expr_types[NodePtr(lit)]` for the `IntTy`, and typeck's `check_expr(tail, sig.ret, …)` populates that entry with sig.ret. **Scope deliberately limited to bare-Expr tails** (CT.1's `parse_expr_stmt` widening); `parse_stmt`'s block-like-statement arm (`KwIf` / `KwWhile` / `KwFor` / `LBrace`) stays unchanged, so `fn classify(x: i32) -> i32 { if x < 0 { -1 } else { 1 } }` still requires explicit `return`, and CT.2d's comptime paren-wrap workaround stays in place. The parser widening + divergent-tail handling rides a separate future sub-bundle so the corpus-regression handling (the `25_if_else.gw` / `27_else_if.gw` / `163_print_padding.gw` shapes where the if's u0 tail-type would mismatch a non-u0 fn return) can be designed separately — the natural answer is "discard u0 tails when fn returns non-u0" but it's deferred for clean staging. The two old E0315 reject tests in `gw_typeck` are replaced with three new tests: two positive (`fn_body_with_int_tail_accepts`, `fn_body_with_arith_tail_accepts`) and one negative (`fn_body_with_tail_type_mismatch_rejects` asserting TYPE_MISMATCH for `fn f() -> u0 { 42 }`). The pre-existing `_clean` tests stay unchanged. | 0 |
| CT.3b | comptime string literals (`CtValue::Str(Vec<u8>)`; `StringLit` decoded via `decode_string_literal` kept in lockstep with `gw_mir`; MIR materialisation as `[]u8` slice aggregate sharing the rodata interning path; `CtValue` loses `Copy`) | `f8bd7df` | +2 phase2_comptime tracers (`ct3b_string_literal.gw` → exit 0 stdout "hi\n", `ct3b_string_escape.gw` → exit 0 stdout "a\tb\n") | `gw_comptime` `CtValue` gains the `Str(Vec<u8>)` variant carrying decoded bytes inline; `#[derive(Copy, Clone, Debug)]` becomes `#[derive(Clone, Debug)]` because `Vec<u8>` isn't `Copy`. The ripple is small and mechanical: `EvalCx::load_local` shifts from `.copied().flatten()` to `.and_then(\|s\| s.as_ref()).cloned()`; `gw_mir::lower_comptime`'s `comptime_values.get(...).copied()` becomes `.cloned()`; the unary-minus operand match and `expect_bool` each gain a `CtValue::Str(_)` rejection arm. `eval_literal` adds an arm for `SyntaxKind::StringLit` decoding via the new `decode_string_literal(raw: &str) -> Vec<u8>` helper, which mirrors `gw_mir::decode_string_literal` byte-for-byte. Supported escapes: `\n \t \r \0 \\ \" \'`; unknown escapes pass through literally (the `\\` + following byte). **`RawStringLit` (`\\…\\` GW syntax) deliberately not handled** — the runtime decoder would mis-decode the `\\` delimiter as the `\\` escape sequence (a latent bug no corpus program exercises); CT.3b stays in lockstep with the validated runtime path. No comptime operations on strings (concat, `==`, `.len`); the tuple-dispatch's existing catch-all `_` rejects them via the `arithmetic_on_string_rejects` test invariant. `gw_typeck` `synth_comptime`'s inner-type gate widens to `Ty::Int(_) \| Ty::Bool \| Ty::Float(_) \| Ty::Slice(IntTy::U8)`; rejection message refreshed to name the new supported set ("`int`, `bool`, `float`, and `[]u8` string blocks only"). `gw_mir` `lower_comptime` signature gains `b: &mut Builder` (the call site in `lower_expr` already has it in scope) so the new arm can build an aggregate. The `(CtValue::Str(bytes), Ty::Slice(IntTy::U8))` arm interns bytes into `lcx.string_literals`, allocates a fresh `Ty::Slice(IntTy::U8)` local, pushes `AssignField data = Const::DataAddr(id)` and `AssignField len = Const::Int(n, USize)`, returns `Operand::Local(dst)`. The materialisation shape is identical to runtime `lower_string_literal`'s; codegen handles both through the same data-pointer + slice-aggregate path. +7 `gw_comptime` unit tests covering: bare literal decoded; empty string; `\n` escape decoded as byte 0x0A; multi-escape payload; unknown escape passes through; string + string rejects via the arithmetic catch-all; **`decode_string_literal_matches_runtime`** — the canonical lockstep assertion against the decoder contract. Storage-decision rationale: owned-inline `Vec<u8>` is simpler than threading a `&mut Vec<Vec<u8>>` storage borrow through every `EvalCx` consumer; the `CtValue: !Copy` change is load-bearing for CT.3c's anticipated `CtValue::Class(Vec<CtValue>)` aggregate shape. | 0 |
| CT.3a | comptime float arithmetic + comparisons (`CtValue::Float(f64)`; float literals; the four arithmetic ops + four orderings + equality dispatch on operand-value tuple; IEEE-754 NaN-aware semantics; `Const::Float` materialisation with f32 narrowing) | `0b3ccba` | +5 phase2_comptime tracers (`ct3a_lt.gw` → 7, `ct3a_add_eq.gw` → 9, `ct3a_negation.gw` → 5, `ct3a_div.gw` → 8, `ct3a_nan_ordering.gw` → 7) | `gw_comptime` `CtValue` gains the `Float(f64)` arm (canonical at `f64`; MIR narrows to `f32` at materialisation if the surrounding `Ty::Float(F32)`). New `EvalError::BadFloatLiteral(Span)` variant kept distinct from `BadIntLiteral` so float-literal decode errors produce a clear diagnostic. `eval_literal` adds a `SyntaxKind::FloatLit` arm parsing via the new `parse_float_literal` helper (mirrors `gw_mir`'s `raw.replace('_', "").parse::<f64>()`, kept in lockstep). `eval_binary` arithmetic / ordering / equality arms refactored to dispatch on the `(lv, rv)` operand tuple instead of routing through the now-removed `expect_int` helper. Each arm accepts both `(Int, Int)` and `(Float, Float)` pairs; mixed pairs and Bool-in-arithmetic combinations reject explicitly via the catch-all `_` so the user sees the type mismatch rather than an arbitrary dominant-type rule (matches the runtime requirement of an explicit `as f64` cast). Float arithmetic uses Rust's IEEE-754 ops directly — `+ - * /` are total, `Slash` / `Percent` by `0.0` yield `±∞` / `NaN` with **no `DivisionByZero` error** (distinct from the integer path, which still raises). Float ordering / equality use Rust's `<` / `<=` / `>` / `>=` / `==` which already implement the IEEE-754 partial-order contract (any comparison involving `NaN` returns `false`, including `NaN == NaN`). `Expr::Unary(Minus)` arm dispatches on the operand value: `Int` via `wrapping_neg`, `Float` via `-f`, `Bool` rejects. `expect_int` removed (its sole consumers — `eval_binary`'s int-only arms — went away in the refactor); `expect_bool` stays. `gw_typeck` `synth_comptime`'s inner-type gate widens from `Ty::Int(_) | Ty::Bool` to `Ty::Int(_) | Ty::Bool | Ty::Float(_)`; rejection message refreshed to name the new supported set; `comptime_error_message` gains an arm for `EvalError::BadFloatLiteral`. `gw_mir` `lower_comptime` adds the `(CtValue::Float(f), Ty::Float(float_ty)) → Const::Float` arm with width-aware bit pattern: `f.to_bits()` for `F64`, `(f as f32).to_bits() as u64` for `F32` — mirrors `lower_literal`'s runtime `FloatLit` path. +21 `gw_comptime` unit tests covering: literal decode (positive + underscored), unary negation, the four arithmetic ops, division-by-zero yielding `+∞`, zero-divided-by-zero yielding `NaN`, Pratt precedence threading, four orderings at simple boundaries + the NaN-ordering case, equality including the `NaN == NaN` is `false` regression proof, mixed-int-float arithmetic / equality rejection, Bool-in-float-arithmetic rejection, `parse_float_literal` underscores + garbage rejection. | 0 |
| block-like-tail | block-like-tail parser widening + divergent-tail discard (widens `parse_stmt`'s block-like arm to leave a bare `Expr` at the enclosing `}`; typeck discards u0 tails when fn returns non-u0; MIR mirrors via `expr_types` lookup) | `9ac51a1` | +3 phase1 fixtures (`204_tail_return_if.gw` → 30, `205_tail_divergent_if.gw` → 22, `206_tail_while_u0.gw` → 0) + 1 phase2_comptime tracer (`ct2d_if_no_paren.gw` → 7, retiring CT.2d's paren-wrap workaround) | `gw_parse` `parse_stmt`'s `KwIf | KwWhile | KwFor | LBrace` arm now mirrors `parse_expr_stmt`'s tail-expression checkpoint trick (CT.1 pattern): parses the expression at a checkpoint and wraps in `ExprStmt` only when the next token is not `RBrace`. At the enclosing block's `}`, the bare `Expr` becomes the block's `tail_expr`. Top-level statements close at `Eof` (not `RBrace`), so module-level shapes are unchanged — only fn-body / nested-block tails are affected. `gw_typeck` `check_fn_body`'s tail handling extends from CT.1's bare-Expr path to all `body.tail_expr()` shapes: synthesise the tail's type; if it's `Ty::U0` and `sig.ret` is non-u0, treat the tail as if it were an `ExprStmt` (discard, no diagnostic). Otherwise run the existing bidirectional `check_expr(tail, sig.ret, &mut cx)`. GW doesn't model `!` (never) explicitly; this rule covers the practical cases — the canonical `if c { return … } else { return … }` whose if-expression naturally types as u0. `gw_mir` `lower_fn` mirrors typeck's discard rule: reads the tail expression's type from `typed.expr_types[NodePtr(tail)]`; if `Ty::U0` while `sig.ret` is non-u0 (and not `Ty::Error`), drops the lowered operand instead of installing it as `Terminator::Return` — the existing `Unreachable` fallback governs, which is correct for divergent tails where the if's join is unreachable in the CFG. Four parse snapshots refreshed (`047_if`, `048_if_else`, `049_else_if`, `050_while`) — the trailing `IfExpr` / `WhileExpr` now appears as a direct child of `Block` (the tail expression) instead of wrapped in `ExprStmt`; the `source:` field also updates from `arsenal_parse` (rename-pass leftover) to `gw_parse`. +3 `gw_typeck` unit tests (`fn_body_with_if_tail_value_accepts`, `fn_body_with_divergent_if_tail_accepts`, `fn_body_with_while_tail_in_u0_accepts`). The existing corpus programs `25_if_else.gw`, `27_else_if.gw`, `163_print_padding.gw` continue to pass — confirms the divergent-tail discard rule preserves their pre-widening semantics. One parser change, two unblocked surfaces — fn-body block-like tails AND CT.2d's comptime paren-wrap workaround (`comptime { if … }` no longer needs the synthetic paren). | 0 |
| CT.2e | comptime short-circuit `&&` / `||` (lazy RHS evaluation, mirrors decision #15's runtime lowering) | `9d062d3` | +5 phase2_comptime tracers (`ct2e_and_short_circuit.gw` → 0, `ct2e_or_short_circuit.gw` → 1, `ct2e_and_eager_rhs.gw` → 5, `ct2e_or_eager_rhs.gw` → 9, `ct2e_short_circuit_guards_let.gw` → 0) | `gw_comptime` `eval_binary` intercepts `SyntaxKind::AmpAmp` / `PipePipe` *before* its eager RHS eval and routes to the new `eval_logical_short_circuit(op, lhs, rhs, span, &mut EvalCx) -> Result<CtValue, EvalError>` helper. The helper evaluates LHS first, pins to bool via `expect_bool`, short-circuits if `l == matches!(op, SyntaxKind::PipePipe)` (the operator's identity element under boolean conjunction / disjunction: `false` for `&&`, `true` for `||`). Otherwise evaluates RHS and returns its bool value. The pattern mirrors decision #15's runtime 3-block CFG (`lower_short_circuit`) compressed into one Rust function since the evaluator owns its own control flow rather than constructing MIR blocks. **Closes CT.2 entirely.** Bool ordering (`true < false`) still rejects via `expect_int`; wildcard `let _` patterns still reject. `gw_typeck` gains no CT.2e-specific code — the existing `synth_binary` `AmpAmp` / `PipePipe` arm eagerly synthesises both operands as `Ty::Bool`, and the eager-vs-lazy divergence is contained inside `gw_comptime`. +7 `gw_comptime` unit tests covering: `&&` eager path (LHS=true); `&&` short-circuit on false LHS (the canonical regression — RHS contains `1 / 0`); `||` eager path (LHS=false); `||` short-circuit on true LHS; **symmetric error-propagation tests** that catch asymmetric "always short-circuit" miscompiles — `&&` with LHS=true MUST propagate the RHS's `DivisionByZero`; same for `||` with LHS=false; non-bool LHS rejects via `expect_bool`. The corpus's `ct2e_and_short_circuit` and `ct2e_or_short_circuit` fixtures are end-to-end mirrors of the short-circuit unit tests; `ct2e_and_eager_rhs` and `ct2e_or_eager_rhs` mirror the eager-path tests; `ct2e_short_circuit_guards_let` exercises the canonical "guard before divide" composition with CT.2c locals (`let n = 0; n != 0 && (10 / n > 5)` — the LHS=false guards the RHS that would otherwise raise `DivisionByZero`). | 0 |
| CT.2d | comptime `if`/`else` (branch-eval discipline, only the taken arm evaluates) | `a03c361` | +5 phase2_comptime tracers (`ct2d_if_true.gw` → 7, `ct2d_if_false.gw` → 99, `ct2d_else_if_chain.gw` → 22, `ct2d_un_taken_safe.gw` → 5, `ct2d_if_with_let.gw` → 20) | `gw_comptime` `eval_expr` gains an `Expr::If(i)` arm calling new `eval_if(IfExpr, span, &mut EvalCx) -> Result<CtValue, EvalError>`. The condition is evaluated via the new `expect_bool(v, span)` helper (parallel to CT.2b's `expect_int`, decision #49) and exactly one arm runs depending on the boolean — the un-taken arm is *never visited*, so any latent side effect inside it (a `1 / 0`, a `let`-init that would otherwise fail, a non-arithmetic op that would otherwise raise `Unsupported`) stays inert. **First comptime sub-bundle where the evaluator's control flow shape diverges from the typed AST's syntactic walk** — typeck's `synth_if` synthesises both arms to confirm their types match; the evaluator walks one. The divergence is contained inside `eval_if` and has no typeck-side counterpart. Else-if chains fall out naturally because `IfExpr::else_branch` returns an `Expr` (either a `Block` for terminal `else { ... }` or another `IfExpr` for chained `else if`), and both shapes are already dispatched by `eval_expr` — chained else-if recurses through `eval_if` via the normal expression match. An `if` without `else` used as a value-producing expression reaches a defensive `Unsupported` arm; typeck rejects this shape before the evaluator runs (the if would be `Ty::U0` and the CT.2c inner-type gate already rejects U0 comptime blocks), but the defensive message keeps the failure mode clear. `gw_typeck` gains no CT.2d-specific code — the existing `synth_if` and `synth_block` paths handle the typed-AST side. +7 `gw_comptime` unit tests covering: if-true takes then arm; if-false takes else arm; un-taken else arm with `1 / 0` doesn't evaluate (the canonical assertion of branch-eval discipline); the symmetric un-taken-then-arm variant; else-if chain dispatch; if with bool result type; integer condition rejects via `expect_bool`. The corpus's `ct2d_un_taken_safe` fixture is the end-to-end mirror of the unit test, exercising the same un-taken-arm-side-effect invariant through both backends. | 0 |
| CT.2c | comptime let-bindings + locals env (`BindingEnv` trait, dense `Vec<Option<CtValue>>` indexed by `BindingId.0`, `NodePtr` moved to `gw_ast::cst`) | `c0d4540` | +4 phase2_comptime tracers (`ct2c_let_simple.gw` → 7, `ct2c_let_chain.gw` → 3, `ct2c_let_shadowing.gw` → 2, `ct2c_let_then_compare.gw` → 4) | **`NodePtr` moved from `gw_typeck` to `gw_ast::cst`** so `gw_comptime` can key into typeck's side-tables without depending on `gw_typeck` (the cycle that would otherwise form: `gw_typeck` → `gw_comptime` → `gw_typeck`). `gw_typeck` keeps a `pub use gw_ast::cst::NodePtr` so existing import paths (e.g. `gw_mir`'s `use gw_typeck::{…, NodePtr, …}`) work unchanged. `gw_comptime` gains a `BindingEnv<'a>` trait with `lookup_pat(NodePtr<'a>) -> Option<u32>` + `lookup_path(NodePtr<'a>) -> Option<u32>` — abstract CST-node → binding-index lookup so the evaluator stays decoupled from `gw_typeck`'s `BindingId` newtype. Zero-sized `NoBindings` resolver provided for CT.1/CT.2a/CT.2b shapes and unit tests. `EvalCx` widened to `EvalCx<'sm, 'env, 'a>` carrying `&dyn BindingEnv<'a>` plus a `Vec<Option<CtValue>>` locals env indexed by `BindingId.0 as usize`. New helpers `store_local(idx, value)` (grows the vec as needed) and `load_local(idx, span)` (returns Unsupported on uninitialised reads — defensive; typeck's name-resolution should make use-before-`let` unreachable). `eval_comptime_block_inner` walks statements: `Stmt::Let` evaluates the init, looks up the pattern's binding index via the resolver, stores. `Stmt::Expr` / `Stmt::Stub` / `Stmt::Error` reject with span-specific Unsupported. New `Expr::Path(p)` arm in `eval_expr` reads from locals via the resolved binding index. `gw_typeck` adds a `TypeckBindingEnv<'a, 'tm>` adapter that borrows the binding maps and converts `BindingId.0` at the trait boundary; borrow scope released before `comptime_values.insert`. +2 net new `gw_comptime` unit tests (`let_without_resolver_rejects` renamed from the old `statement_in_block_is_unsupported`; new `path_without_resolver_rejects` covers the `Expr::Path` rejection arm). | 0 |
| CT.2b | comptime comparisons + booleans (`CtValue::Bool`, four ordering ops, overloaded equality, op-first dispatch via `expect_int`) | `d9f8064` | +4 phase2_comptime tracers (`ct2b_lt.gw` → 1, `ct2b_lt_false.gw` → 0, `ct2b_eq_bool.gw` → 2, `ct2b_arith_compare.gw` → 3) | `gw_comptime` `CtValue` gains `Bool(bool)` arm; `eval_literal` recognises `KwTrue` / `KwFalse`. `eval_binary` reorganised around op-first dispatch with three groups: arithmetic (CT.2a — `+ - * / %`), integer ordering (CT.2b — `Lt, LtEq, Gt, GtEq`), and equality (CT.2b — `EqEq, BangEq` overloaded for both `(Int, Int)` and `(Bool, Bool)` pairs). Mixed-type equality (e.g. `1 == true`) rejects explicitly with `EvalError::Unsupported` rather than inventing a dominant-type rule. New `expect_int(v: CtValue, span: Span) -> Result<i128, EvalError>` canonical operand-type helper routes arithmetic / ordering / `Unary(Minus)` through a single rejection site — same pattern as decisions #38 (`is_aggregate_ty`) and #40 (`wrap_to_optional_if_needed`). Bool ordering (`true < false`) and logical `&&` / `||` (`AmpAmp` / `PipePipe`) flow through the evaluator's outer `_` arm with a clear "this operator is not yet supported" Unsupported diagnostic so deferred ops produce a clean rejection rather than a wrong answer. `gw_typeck` `synth_comptime` inner-type gate widens from `Ty::Int(_)` to `Ty::Int(_) \| Ty::Bool`; rejection message names the supported set ("`int` and `bool` blocks only"). `gw_mir` `lower_comptime` gains the `(CtValue::Bool, Ty::Bool) → Const::Bool(b)` arm — the first new materialisation arm since CT.1. All existing `let CtValue::Int(n) = …` test destructures refactored to go through `assert_int` / `assert_bool` helpers (flagged inline in CT.2a's doc comment). +12 `gw_comptime` unit tests covering true / false literals, the four ordering ops at boundary and non-boundary cases, int and bool equality / inequality, negated-operand interaction with comparison, arithmetic-on-bool rejection, bool-ordering rejection, mixed-type equality rejection, and the catch-all rejection of `&&`. | 0 |
| CT.2a | comptime integer arithmetic (`+ - * / %` over `i128`, IntegerOverflow / DivisionByZero error variants) | `ce5ada5` | +4 phase2_comptime tracers (`ct2a_add.gw` → 3, `ct2a_precedence.gw` → 7, `ct2a_div_mod.gw` → 16, `ct2a_negation.gw` → 5) | `gw_comptime` `eval_binary(BinaryExpr, Span, &mut EvalCx) -> Result<CtValue, EvalError>` handles `Plus / Minus / Star / Slash / Percent`; operand types pinned to `CtValue::Int` via irrefutable let-destructure (CT.2b's `Bool` addition will refactor to a match — flagged inline); arithmetic uses `i128::checked_{add,sub,mul,div,rem}` with overflow → `EvalError::IntegerOverflow(span)`; `Slash`/`Percent` short-circuit on `rhs == 0` to `EvalError::DivisionByZero(span)` so the two failure modes never confuse diagnostically. The `_` arm of the operator match returns `Unsupported` so a user who writes `comptime { 1 < 2 }` today gets a clear message naming CT.2b rather than a wrong answer. `gw_typeck` `comptime_error_message` gains arms for the two new variants — both route through the existing E0314 diagnostic. +9 `gw_comptime` unit tests covering the five binary ops, Pratt precedence threading the evaluator without special handling, negated-operand interaction with CT.1's `Unary(Minus)` arm, division-by-zero, modulo-by-zero, and the graceful Unsupported diagnostic for non-arithmetic ops. Replaced the obsolete `arithmetic_is_unsupported_at_ct1` test with `binary_addition`. | 0 |
| CT.1 | comptime tracer (integer-literal blocks, typeck-side evaluator, tail-expr CST shape, E0315 guard rail) | `018d4eb` | +1 phase2_comptime tracer (`ct1_tracer.gw` → exit 4) + 1 lexparse fixture (`062_comptime_tail_expr.gw`) | parser `parse_comptime_expr` invoked from `parse_atom` on `KwComptime`; `parse_expr_stmt` widened to leave a bare `Expr` child when at `}` without `;` (the `parse_stmt:538` LBrace / KwIf / KwWhile / KwFor arm is unchanged — block-like stmts still wrap in `ExprStmt`); AST `Expr::Comptime(ComptimeExpr)` promoted from `Stub` with `block()` accessor; new `gw_comptime` crate (was a doc-only stub) becomes active — tree-walking interpreter on the typed AST with `CtValue::Int(i128)`, `EvalCx` / `Budget` (10⁹ steps, 1024 depth), `EvalError::{Unsupported, BudgetExceeded, StackOverflow, BadIntLiteral}`, `eval_comptime_block`; typeck `synth_comptime` synthesises the inner block's type then runs the evaluator and stashes the result in new `TypedModule::comptime_values: FxHashMap<NodePtr, CtValue>`; new error codes E0314 `COMPTIME_EVAL_FAILED` and E0315 `TAIL_EXPR_IN_FN_BODY` (the latter rejects fn bodies whose `Block::tail_expr()` is set, regardless of return type — turns the latent `lower_fn` discard-and-trap into a clear compile-time error); MIR `lower_comptime` pulls the stashed `CtValue` and emits `Operand::Const(Const::Int { value, ty })` directly, never lowering the block body. +9 `gw_comptime` unit tests covering literal / negated / paren / nested-block / hex / statement-reject / arithmetic-reject / step budget / recursion depth; +5 `gw_typeck` unit tests covering the E0315 guard rail; +2 `gw_driver` integration tests (one per backend, walking `tests/corpus/pass/phase2_comptime/`) | 0 caught + **1 deferred via E0315** (latent: `lower_fn:657` discards the tail operand from `lower_block`, then fabricates either `Return(Unit)` for `u0` or `Unreachable` (trap) for non-`u0` — turning `fn f() -> i32 { 42 }` into a silent runtime trap. The E0315 diagnostic guards the shape until implicit-tail-return ships as its own sub-bundle that wires the operand into `Terminator::Return`.) |

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

**O.2 confirms the recombination rule.** Pure recombination of
M.x's decision-tree + O.1's Optional aggregate machinery. Pre-bundle
prediction: ~1 bug. Observed yield: 0. The new pattern shape
(`nil` arm) introduces no new value-level surface: the tag-byte
read was already validated through `??` (O.1), and the
chain-of-Branch shape was already validated through M.x.

**O.3 generalises the O.1 lesson and finds two new bugs.** Pre-
bundle prediction: ~1-2 bugs. Observed yield: 2 — both *latent
O.1 bugs* that the O.3 tracer happened to surface: (a) LLVM
`make_fn_type` had a hardcoded `Class \| Slice` allow-list for
aggregate fn returns; the O.1 tracer didn't use a `?T`-returning
fn, so the bug stayed dormant. (b) MIR `lower_return` didn't
apply the `T → ?T` / `T → !T` wrap when returning a bare `T`
from an aggregate-typed fn — same pattern as the existing
`lower_let` wrap, just at a different syntactic site. Both fixes
generalise the O.1 lesson: every site that switches on
T-vs-aggregate-tagged-T should go through one canonical
`wrap_to_optional_if_needed` helper, the same way every site
that switches on aggregate-vs-primitive must go through
`is_aggregate_ty`. Three sites now do (`lower_let`,
`lower_return`, `lower_call`); future variants like
`Result<T, E>` or sum types should auto-handle.

**F.1 / F.2 / F.3 confirm the organisational-novelty rule.**
The modules sub-bundle is purely organisational: cross-file
resolve, opt-in module declarations, per-file `use` scoping. No
new MIR shape, no new aggregate layout, no new value-level
lowering. Pre-bundle prediction by the 12/A.x heuristic was
~3 across the three sub-bundles (cross-file resolve is
shape-novel; per-file scoping is shape-novel for typeck). Observed
yield: 0 across all three. The heuristic refines further: even
shape-novel surfaces stay clean when their value-level lowering
is identical to existing paths. F.x's "novelty" is entirely
table-manipulation: more defs in one vec, file-keyed scope maps
that consult the same `by_name` storage, typeck name lookups
that read from a different map. The dual-backend test had
nothing new to disagree about because MIR/codegen saw exactly
the same shapes as single-file builds.

The one bit of *latent* organisational risk: F.2's global-import
shortcut (each `use foo;` flattens into `by_name`) was
anti-modular but caught by F.3 as a deliberate refinement, not
a bug. The session's own design decisions were tracked
explicitly via the user-question / recommend / wait-for-go
loop, so F.2's compromise was a known tradeoff rather than a
silent miscompile.

**CT.1 sharpens the lesson once more.** The comptime tracer
introduces a brand-new evaluator (no parallel implementation to
crib from), a new aggregate-free `CtValue` type, and a new
typeck-side hookpoint where the result is materialised into the
typed AST rather than MIR. Pre-bundle prediction by the
12/A.x heuristic was ~2-3 bugs. **Observed yield: 0 caught + 1
deferred via E0315.** The "0 caught" half is consistent with the
recombination rule — the integer-literal evaluator is small
enough that the dual-backend test had nothing new to disagree
about (MIR sees only the final `Const::Int`, not the evaluator's
internal state). The "1 deferred" half is the more interesting
data point: the *parser* widening that CT.1 introduces
(tail-expression form) creates a latent miscompile shape
(`fn f() -> i32 { 42 }` → `lower_fn` discards the tail operand
and traps via `Unreachable`) that the tracer alone would never
have exercised. The audit-then-fix loop surfaced it explicitly,
and the chosen fix was a typeck-side reject (E0315
`TAIL_EXPR_IN_FN_BODY`) rather than a value-level fix, deferring
the implicit-tail-return widening to its own sub-bundle. This
extends the heuristic: bug prediction should weight not just
the evaluator's value-level novelty but also any *latent* shapes
the necessary parser/AST changes accidentally unlock — even when
those shapes aren't part of the sub-bundle's stated scope.

**CT.2a confirms the refined heuristic again.** Integer
arithmetic inside the evaluator is value-level-novel only inside
`gw_comptime` itself — the operators land, the `EvalError` enum
grows two variants, the typeck-side error-message table grows two
arms. But the *materialisation* shape at the typeck/MIR boundary
is unchanged: `synth_comptime` still stashes a `CtValue::Int(i128)`,
`lower_comptime` still emits `Operand::Const(Const::Int)`. MIR and
codegen see no new shape, so the dual-backend invariant has
nothing new to disagree about. Pre-bundle prediction: ~0-1.
**Observed yield: 0 caught.** The 9 new `gw_comptime` unit tests
(five ops, precedence, negated-operand interaction with CT.1's
`Unary(Minus)`, division-by-zero, modulo-by-zero, Unsupported
diagnostic for non-arithmetic ops) and the 4 new corpus programs
(× 2 backends through the existing directory walker) form the
regression net. The bug surface this re-arms is CT.2b, where
`CtValue::Bool` lands as a new variant and the materialisation
boundary genuinely changes shape — `lower_comptime` will need a
new arm for `(CtValue::Bool, Ty::Bool) → Const::Bool(b)`, and
that's the first place where two backends could disagree.

**CT.2b refines the heuristic further.** Pre-bundle prediction
was ~1: the new `Const::Bool` materialisation arm is the first
real shape change at the typeck/MIR boundary since CT.1, so
dual-backend parity actually had something to verify here.
**Observed yield: 0 caught.** Two compounding reasons drop the
yield below prediction: (a) the new `Const::Bool` arm reuses the
existing runtime `Const::Bool` lowering already exercised by
phase1 bool literals across both backends (`KwTrue` /
`KwFalse` → `Const::Bool` has been validated since increment 6
of Phase 1) — codegen gets zero new arms across either backend,
so "new materialisation shape at the comptime boundary" turns
out to mean "the same materialisation shape that runtime bool
literals already use, just reached via a different
synthesis path"; (b) the canonical `expect_int` operand-type
predicate (decision #49) collapses every ill-typed bool-in-
arithmetic / mixed-type-equality / bool-ordering case into the
same Unsupported diagnostic shape, so typeck never reaches
lowering for the ill-typed cases and MIR / codegen see only
well-typed inputs. The heuristic refines once more: when a new
materialisation arm reuses a *runtime* lowering that's already
been validated across both backends, the dual-backend test has
nothing new to disagree about even when the typeck/MIR boundary
shape genuinely changed. The remaining comptime bug surface is
CT.2c — *evaluator state* (let-bindings, branches, locals env)
that has no parallel implementation to crib from and that the
existing dual-backend test cannot catch (the typed AST never
sees the evaluator's internal locals).

**CT.2c lands the predicted evaluator-state surface and yields
zero bugs.** Pre-bundle prediction was ~1-2 — the largest
predicted surface since O.3, because CT.2c introduces (a) the
first instance of evaluator state that outlives a single
expression (the `Vec<Option<CtValue>>` locals env), (b) a new
abstraction surface at the gw_comptime ↔ gw_typeck boundary
(the `BindingEnv` trait + `TypeckBindingEnv` adapter), and
(c) a structural move (`NodePtr` relocated from gw_typeck to
gw_ast to break the dep cycle that would otherwise form). The
dual-backend test catches *materialisation* bugs but cannot
catch evaluator-state bugs — the typed AST never sees the
locals, so MIR and codegen see only the final `Const::Int` or
`Const::Bool` regardless of how the evaluator got there.
**Observed yield: 0 caught.** Three factors collapse the
prediction: (a) Q5 option (a) — `Vec<CtValue>` indexed by
`BindingId.0` — trivially matches typeck's existing dense
BindingId allocation, so the data-structure choice introduces
no impedance mismatch between typeck's allocator and the
evaluator's storage (a `FxHashMap` or stack-of-frames
alternative would have introduced one); (b) the let-store
and path-load operations have no branching, no scope nesting,
no short-circuit discipline — they're literal one-line
operations on the locals vec; (c) the abstraction boundary
(`BindingEnv`) was tested first against the rejection path
(NoBindings → Unsupported diagnostic via the corpus's
shadowing fixture) before the happy path landed, so any
adapter-side off-by-one would have surfaced in a way the
unit tests catch directly. The corpus shadowing fixture
specifically guards against name-vs-index confusion: `let x =
1; let x = x + 1; x` → 2 *only if* the second `let`'s init
sees the first `let`'s value (the freshly-allocated BindingId
hasn't been written yet at the moment the rhs evaluates).
**CT.2d will be the next re-armed prediction.** `if`/`else`
over `CtValue::Bool` is the first sub-bundle where the
evaluator's control flow shape diverges from the syntactic
shape — only the taken arm evaluates, branch-eval discipline
matters, the locals env needs to handle a value defined in
one branch and read after the join. That's where the
single-arm-evaluation invariant could break in a way
neither the dual-backend test nor a simple corpus tracer
would catch unless the corpus deliberately exercises the
not-taken-arm-has-side-effects shape.

**CT.2d lands the predicted control-flow surface and yields
zero bugs.** Pre-bundle prediction was ~1 — branch-eval
discipline is the first comptime feature whose correctness
*cannot* be verified by the dual-backend test (the typed AST
exposes both arms, MIR sees only the materialised constant,
so neither backend would ever disagree about a
both-arms-evaluated miscompile that happened to produce the
correct value via the wrong path). **Observed yield: 0
caught.** Two factors collapse the prediction: (a) the
implementation is a single Rust `if` statement — `if cond_b {
eval_then } else if let Some(else_branch) = ... { eval_else
}` — with no nested branching inside the implementation that
could go wrong; the discipline is structural, not a check
that has to be remembered at each evaluation step. (b) The
critical regression test (`ct2d_un_taken_safe`: `(if true {
5 } else { 1 / 0 })` → 5, where the else arm would raise
`DivisionByZero` if evaluated) and its unit-test sibling
(`if_un_taken_arm_is_not_evaluated`) catch the only
realistic miscompile shape — accidentally walking both arms
— directly, so any future regression that breaks the
discipline would fail loudly. The symmetric
`if_un_taken_then_arm_is_not_evaluated` test pairs them so
an asymmetric "only evaluate else arm" miscompile would also
trip. CT.2d's "this is the test that's hard to write" warning
applies in general but the small surface area here made it
straightforward to cover. **CT.2e is the next predicted
surface and the smallest remaining comptime sub-bundle**:
logical `&&` / `||` with lazy evaluation is pure recombination
of CT.2b's bool dispatch and CT.2d's branch-eval discipline
— estimated yield ~0.

**CT.2e confirms the recombination rule and closes CT.2.**
Pre-bundle prediction was ~0 (pure recombination of CT.2b's
`expect_bool` and CT.2d's branch-eval discipline applied at
the operator level rather than the statement level).
**Observed yield: 0 caught.** The implementation is a
single-page Rust function whose behaviour mirrors decision
#15's runtime lowering pattern verbatim: LHS first, short-
circuit if the LHS is the operator's identity element under
boolean conjunction / disjunction (`false` for `&&`, `true`
for `||`), otherwise evaluate RHS and return its bool. The
asymmetric error-propagation tests
(`and_propagates_rhs_error_when_lhs_true` and
`or_propagates_rhs_error_when_lhs_false`) were the
heuristic's main risk-reduction insurance against a "treat
both arms as short-circuit candidates" miscompile that an
all-correct-LHS suite of unit tests wouldn't catch; both
pass on first try because the implementation cleanly
parameterises the short-circuit value via `matches!(op,
SyntaxKind::PipePipe)` rather than per-operator special
cases. **The recombination rule holds**: when a new
sub-bundle's value-level surface is reachable as a Cartesian
product of two already-validated surfaces (CT.2b's
`expect_bool` + CT.2d's "only evaluate the determining
path"), the dual-backend test has nothing new to disagree
about and the unit tests are mostly there to lock in the
expected composition. CT.2 closes with 0 caught + 1 deferred
across five sub-bundles, against an aggregate prediction of
~4 — a 4× under-shoot that reflects how much of CT.2's
"novelty" was actually recombination of decisions baked in
during Phase 1 + the CT.x design phase.

The remaining Phase-2 work is CT.3 (wider types in the
`CtValue` enum — `Float`, strings, classes, optionals as the
corpus motivates). The block-like-tail parser widening + the
divergent-tail discard rule landed in commit `9ac51a1`,
retiring the CT.2d paren-wrap workaround in the same change.

**Implicit-tail-return (bare-Expr scope) lands cleanly.**
Pre-bundle prediction by the 12/A.x heuristic was ~1:
implicit-tail-return adds a new value-level path through
`lower_fn` (capture the lowered tail operand and install it
as the `Terminator::Return` value) that the existing fn-body
lowering didn't exercise, plus a typeck/MIR boundary change
(the narrowed `expr_types[tail]` entry has to be consulted
by `lower_literal` for the materialised constant to carry
the right `IntTy`). **Observed yield: 0 caught.** Two
factors collapse the prediction: (a) the narrowing path was
already validated by the `let x: i64 = 42` shape across
hundreds of phase1 corpus programs — `expr_types[tail] =
sig.ret → lower_literal picks up the IntTy` is just a new
call site for an existing mechanism, not a new
implementation. (b) the MIR change is a single Rust `if`
(use the captured operand iff a tail was present), matching
the CT.2d pattern of "structural discipline rather than a
check that has to be remembered." The four corpus fixtures
exercise four distinct points on the bidirectional-narrowing
surface (literal tail, arithmetic tail, let-then-path tail,
widened-literal tail); the `203_tail_return_widening` fixture
specifically guards against a "tail position defaults to
i32" miscompile by returning `i64` (100 narrows to i64
correctly only if the new tail-type check consults sig.ret
rather than defaulting). The deliberate scope limit
(bare-Expr only, no parser widening) avoids the
divergent-tail corpus-regression surface that the full
shape would have introduced; the corpus + heuristic stay
clean as a result. CT.1's E0315 deferred bug is now resolved
— the canonical Rust-style `fn add(a, b) -> i32 { a + b }`
shape works end-to-end. **The Phase 2 CT.1 prediction
"latent-shape risk from parser side-effects" is now
fully discharged.**

### What 279 corpus programs cover

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
- Phase 2 increment F.1 / F.2 / F.3 surface (lives in the new
  `tests/corpus/pass/phase2_multifile/` directory, exercised
  through the new `phase2_multifile.rs` integration test): four
  multi-file projects exercising flat-namespace cross-file calls
  (`01_add_two_files`), cross-file class layout (`02_cross_file
  _class`), `mod` + global `use` (`03_mod_use`), and
  per-file `use` scoping (`04_use_per_file` — three files, both
  main and lib `use math;` independently). Each project is
  staged into a per-test temp dir so the auto-discovery doesn't
  pull in the F.1 corpus's own siblings.
- Phase 2 increment O.1 / O.2 / O.3 surface: `?T` optional and
  `!T` error-union types for primitive inners (`?i32`, `?i64`,
  `?bool`, `!i32`, `!bool`, …) at `let` annotations, fn signatures
  (param + return), and call-arg slots; `nil` literal adopts `?T`
  from any Optional context; bare `T` coerces to `?T` and `!T` at
  any assignable position via a single canonical wrap helper that
  fires at three sites (`lower_let`, `lower_return`, `lower_call`).
  Reverse directions (`?T → T`, `!T → T`, `?T ↔ !T`) rejected —
  user must unwrap. `??` coalesce operator: reads the LHS optional's
  tag byte, returns the payload if `tag == 1`, lazily evaluates the
  RHS default if `tag == 0`. `expr!` postfix assert: reads the LHS
  error-union's tag byte, returns the payload if `tag == 1`, traps
  on `tag == 0` (via `Terminator::Unreachable` which both backends
  lower as a hardware trap). `match opt { nil => …, _ => … }`
  exercises the existing decision-tree pattern infrastructure with
  a tag-byte read in the `nil`-arm test. Both `?T` and `!T`
  aggregates lower as `{ tag: u8 @ 0, payload: T @ align_of(T) }`,
  total size aligned to the inner's alignment (`?i32` / `!i32` =
  8 bytes, `?i64` / `!i64` = 16, `?bool` / `!bool` = 2). Both
  backends agree byte-for-byte.
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
- Phase 2 increment CT.1 + CT.2a + CT.2b + CT.2c + CT.2d +
  CT.2e + CT.3a + CT.3b surface (lives in
  `tests/corpus/pass/phase2_comptime/`, exercised through the
  `phase2_comptime.rs` driver integration test, which walks
  the directory and runs every `.gw` through both
  `--backend=fast` and `--backend=llvm` as two separate
  `#[test]` fns). Thirty-one tracer programs today: `ct1_tracer.gw` → exit
  4 (CT.1 — bare integer literal inside `comptime { N }`);
  `ct2a_add.gw` → 3 (`comptime { 1 + 2 }`, the canonical CT.2a
  tracer bullet); `ct2a_precedence.gw` → 7 (`comptime { 1 + 2 *
  3 }`, proves Pratt precedence flows through the evaluator
  without special handling — the AST nests correctly and the
  evaluator just walks); `ct2a_div_mod.gw` → 16 (`comptime { 100
  / 7 + 100 % 7 }` = 14 + 2, exercises division truncation
  toward zero and modulo); `ct2a_negation.gw` → 5 (`comptime {
  10 - 3 - 2 }`, left-associative subtraction); `ct2b_lt.gw` →
  1 (`if comptime { 1 < 2 } { return 1; } else { return 0; }`,
  the canonical CT.2b tracer bullet — proves the new
  `Const::Bool` materialisation arm reaches both backends);
  `ct2b_lt_false.gw` → 0 (`if comptime { 2 < 1 } { return 1; }
  else { return 0; }`, the false-branch counterpart so the
  `Bool(false)` arm is exercised, not just `Bool(true)`);
  `ct2b_eq_bool.gw` → 2 (`if comptime { true == true } { return
  2; } else { return 0; }`, the overloaded-equality `(Bool,
  Bool)` path); `ct2b_arith_compare.gw` → 3 (`if comptime { 1 +
  2 * 3 == 7 } { return 3; } else { return 0; }`, CT.2a +
  CT.2b composition through Pratt precedence);
  `ct2c_let_simple.gw` → 7 (`comptime { let x = 7; x }`, the
  canonical CT.2c tracer bullet); `ct2c_let_chain.gw` → 3
  (`comptime { let x = 1; let y = 2; x + y }`, two bindings +
  arithmetic recombines CT.2a); `ct2c_let_shadowing.gw` → 2
  (`comptime { let x = 1; let x = x + 1; x }`, shadowing
  allocates a fresh BindingId so the second `let`'s init sees
  the first binding's value — proves the index-based locals env
  handles name re-use correctly); and
  `ct2c_let_then_compare.gw` → 4 (`if comptime { let n = 5; n <
  10 } { return 4; } else { return 0; }`, let composed with
  CT.2b comparison); `ct2d_if_true.gw` → 7 (`comptime { (if 1 <
  2 { 7 } else { 0 }) }`, the canonical CT.2d tracer bullet);
  `ct2d_if_false.gw` → 99 (`comptime { (if 1 > 2 { 7 } else {
  99 }) }`, false-branch counterpart); `ct2d_else_if_chain.gw`
  → 22 (`comptime { (if false { 1 } else if true { 22 } else
  { 99 }) }`, exercises the recursive `else_branch` returning
  `Expr::If`); `ct2d_un_taken_safe.gw` → 5 (`comptime { (if
  true { 5 } else { 1 / 0 }) }`, **the critical branch-eval
  regression test** — the else arm `1 / 0` would raise
  `EvalError::DivisionByZero` if evaluated, so a passing exit
  5 is proof that the un-taken arm was never visited); and
  `ct2d_if_with_let.gw` → 20 (`comptime { let x = 10; (if x <
  100 { x * 2 } else { 0 }) }`, composition of CT.2c locals
  with CT.2d branches); `ct2e_and_short_circuit.gw` → 0
  (`if comptime { false && (1 / 0 == 0) } { return 1; } else
  { return 0; }`, **the canonical CT.2e regression test** —
  RHS would raise `EvalError::DivisionByZero` if evaluated, so
  exit 0 proves short-circuit fired); `ct2e_or_short_circuit.gw`
  → 1 (`if comptime { true || (1 / 0 == 0) } { return 1; }
  else { return 0; }`, symmetric `||`); `ct2e_and_eager_rhs.gw`
  → 5 (`if comptime { true && (3 > 2) } { return 5; } else
  { return 0; }`, LHS=true so RHS evaluates and determines
  result — catches asymmetric "always short-circuit"
  miscompile); `ct2e_or_eager_rhs.gw` → 9 (symmetric `||`
  eager path); and `ct2e_short_circuit_guards_let.gw` → 0
  (`if comptime { let n = 0; n != 0 && (10 / n > 5) }`, the
  canonical "guard before divide" composition with CT.2c
  locals — `n != 0` LHS=false short-circuits before the `10
  / n` RHS that would otherwise raise `DivisionByZero`).
  `ct3a_lt.gw` → 7 (`comptime { if 1.5 < 2.5 { 7 } else { 0 } }`,
  the canonical CT.3a float-ordering tracer); `ct3a_add_eq.gw`
  → 9 (`comptime { if 0.5 + 1.5 == 2.0 { 9 } else { 0 } }`,
  float arithmetic + equality — `0.5 + 1.5` is exact in
  IEEE-754 so the `== 2.0` doesn't trip on rounding);
  `ct3a_negation.gw` → 5 (float `Unary(Minus)` path);
  `ct3a_div.gw` → 8 (exact float division `6.0 / 2.0 == 3.0`);
  and `ct3a_nan_ordering.gw` → 7 (**the canonical IEEE-754
  NaN regression test** — `0.0 / 0.0 < 1.0` yields `false`
  because any ordering comparison involving NaN returns
  false per IEEE-754, so the `if` takes the `else` arm; also
  proves `0.0 / 0.0` does NOT raise `DivisionByZero` for the
  float path, distinct from the integer divide-by-zero arm
  which still raises). `ct3b_string_literal.gw` → exit 0 with
  stdout "hi\n" (`let s: []u8 = comptime { "hi\n" };` +
  `write(1, s.data, s.len)` — the canonical CT.3b tracer
  pinning the comptime slice materialisation shape on both
  backends); `ct3b_string_escape.gw` → exit 0 with stdout
  "a\tb\n" (multi-escape payload pinning the decoder lockstep
  with `gw_mir::decode_string_literal`). The accepted shapes inside the block
  are zero-or-more `let pat = init;` statements with
  `IdentPat` patterns, followed by a tail expression of shape
  `IntLit / FloatLit / StringLit / KwTrue / KwFalse / Path
  (resolved to a let-bound local) / Paren(expr) / Unary(Minus,
  expr) / Block(of-same) / Binary(lhs, op, rhs) / IfExpr` for
  `op ∈ {+, -, *, /, %, <, <=, >, >=, ==, !=, &&, ||}`
  (arithmetic / ordering / equality accept both `(Int, Int)`
  and `(Float, Float)` pairs since CT.3a; equality also
  accepts `(Bool, Bool)` from CT.2b; strings are opaque
  payloads — no comptime ops on them in CT.3b; `&& / ||`
  short-circuit lazily;
  bool ordering and mixed int/float pairs are deferred).
  `if`/`else` at the comptime block's tail is now accepted
  directly without paren-wrapping (block-like-tail widening,
  commit `9ac51a1`); the historical CT.2d paren-wrap workaround
  is retired and the `ct2d_if_no_paren.gw` fixture pins the new
  shape. The pre-existing CT.2d paren-wrapped fixtures still
  parse fine — the new tail shape is additive. CT.2e fixtures
  did not need paren-wrapping even pre-widening because the
  surrounding `if` was *outside* `comptime`, leaving the binary
  `&&` / `||` as the block's tail expression. Wildcard `_`
  patterns in `let`, bool ordering, and expression statements
  still reject with `EvalError::Unsupported`. The 62-program lex+parse snapshot corpus also
  has a `062_comptime_tail_expr.gw` fixture (from CT.1) locking
  the `ComptimeExpr` CST shape *and* the bare-Expr tail child
  inside the comptime block; the outer fn body still uses
  `ExprStmt` (because of `return …;`'s trailing `;`), so this
  fixture intentionally does *not* commit the codebase to `fn
  { tail }` semantics — that's deferred to a future sub-bundle.
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
- Phase 2 implicit-tail-return (bare-Expr scope) surface (lives
  in `tests/corpus/pass/phase1/`, exercised through `phase1_run.rs`
  + `llvm_backend.rs`): four fixtures locking the canonical
  Rust-style fn-body shapes. `200_tail_return_arith.gw` → exit 7
  (`fn add(a: i32, b: i32) -> i32 { a + b }`); `201_tail_return
  _literal.gw` → exit 42 (`fn answer() -> i32 { 42 }`, the
  simplest bare-literal); `202_tail_return_let_then_path.gw` →
  exit 22 (let-bound local read by a path-expression tail);
  `203_tail_return_widening.gw` → exit 100 (i64 tail narrowed
  from the bare literal `100` via the bidirectional rule —
  catches a "tail position defaults to i32" miscompile). The
  accepted shape is "fn body ending with an expression that has
  no trailing `;`" where the expression types as the fn's
  declared return type. Block-like tails (`if` / `while` / `for`
  / `{ … }` at fn-body end without `;`) still wrap in `ExprStmt`
  at the parser level and require explicit `return`; that
  widening rides a future sub-bundle.

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
bit-exactly the same value across all 279 single-file corpus
programs (248 phase1 + 31 phase2_comptime) + 4 multi-file
projects.

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

The Phase-2 `!T` surface (O.3) brings the parallel error-union
shape with postfix `!`-assert:

```gw
fn safe_compute(seed: i32) -> !i32 {
    return seed * 3;
}

fn main() -> i32 {
    let r: !i32 = safe_compute(7);
    return r!;
}
```

Exit code: 21. The fn return wraps `seed * 3 = 21` into `{tag: 1,
payload: 21}` (the `T → !T` coercion fires at `lower_return`), and
`r!` reads the payload field (tag = 1 → ok branch). On a hypothetical
`tag = 0` (err) value, the `!` postfix would trap via
`Terminator::Unreachable`. Both backends produce 21 byte-for-byte.

The Phase-2 comptime surface (CT.1 + CT.2a + CT.2b + CT.2c +
CT.2d + CT.2e + CT.3a + CT.3b — **CT.2 closed, CT.3 underway**)
brings compile-time evaluation of integer-, bool-, float-, and
`[]u8`-string-typed blocks with let-bindings, full control flow,
short-circuit logical operators, IEEE-754 float arithmetic /
comparisons, and string-literal materialisation via the shared
runtime rodata path. The
canonical "guard before divide" pattern:

```gw
fn main() -> i32 {
    if comptime { let n = 0; n != 0 && (10 / n > 5) } { return 1; } else { return 0; }
}
```

Exit code: 0. typeck walks the block: the `let n = 0`
allocates `BindingId(0)`; the `n != 0` path expression
populates `path_bindings`; the surrounding `&&` synthesises
both operands to `Ty::Bool`. `gw_typeck` constructs a
`TypeckBindingEnv` and runs `gw_comptime::eval_comptime_block`.
The evaluator stores `0` at index 0, evaluates the LHS `n !=
0` → `CtValue::Bool(false)`, **short-circuits before
evaluating the RHS** — the `10 / n` (which would raise
`EvalError::DivisionByZero`) is never visited. The block's
tail value is `CtValue::Bool(false)`. MIR's `lower_comptime`
emits `Operand::Const(Const::Bool(false))` directly; the
outer `if`'s condition is the constant false, so codegen
lowers an unconditional `Goto` to the else arm. Both backends
produce exit 0 byte-for-byte. The accepted shapes today are
zero-or-more `let` statements with `IdentPat` patterns
followed by a tail expression of shape `IntLit / KwTrue /
KwFalse / Path / Paren(expr) / Unary(Minus, expr) /
Block(of-same) / Binary(lhs, op, rhs) / IfExpr` for `op ∈
{+, -, *, /, %, <, <=, >, >=, ==, !=, &&, ||}` (`&&` / `||`
short-circuit lazily). `if` at the comptime block's tail now
parses directly without paren-wrapping (`comptime { if c { a }
else { b } }`) following the block-like-tail widening — the
historical CT.2d paren-wrap workaround is retired.

### Driver UX

```bash
$ gw new hello
created project `hello`:
  hello/build.gw       # Phase 2 manifest (currently has Phase-2 syntax)
  hello/hello.gw            # spec §5.15.1 hello world (currently rejected)
$ gw build path/to/some.gw
built `path/to/some`
$ ./path/to/some
$ echo $?
21
$ gw build --backend=llvm path/to/some.gw   # Phase 13
built `path/to/some`
$ gw dump path/to/some.gw     # AST dump for debugging
$ gw --version
gw 0.0.1
```

### Test infrastructure

- `cargo test` at workspace root runs the entire suite (148 tests).
- `cargo test -p gw_parse --test corpus` runs the lex+parse
  insta snapshot corpus (61 pass, 5 fail).
- `cargo test -p gw_driver --test phase1_run` runs every
  `tests/corpus/pass/phase1/*.gw` end-to-end through the
  Cranelift backend: builds, executes, matches exit code (and stdout
  where `.expected_stdout` is present). Skipped on Windows
  (`#![cfg(not(windows))]`) — `cc` integration is a later concern.
- `cargo test -p gw_driver --test llvm_backend` runs the **same
  226-program corpus** through `gw build --backend=llvm`. Both
  tests share the corpus directory; any program added to
  `tests/corpus/pass/phase1/` is automatically exercised through
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
  a dev install). Restore Windows when either `gw_codegen_llvm`
  is feature-gated or a working Windows install path emerges.

---

## What doesn't work yet (Phase-1-deferred or incomplete)

| Limitation | Surface | Path forward |
|---|---|---|
| Raw pointers outside `extern fn` signatures | Typeck rejects `*T` in non-extern fn params/returns | Memory-model + borrow-checker work (Phase 3); also blocks meaningful pointer arithmetic |
| Nested class fields | Typeck rejects | Generalise size/offset computation in `resolve_class_layout`; recurse on `Ty::Class` field types |
| Slice-typed class fields | Typeck rejects | Class layout would need to embed the slice's `(data, len)` pair |
| Non-`u8` slice element types | Typeck rejects `[]i32` etc. (only `[]u8` accepted today) | Generalise the slice arm in `resolve_type`; aggregate_layout already handles arbitrary 8-byte fields, so codegen mostly follows |
| Generics, `trait`, async | Parser produces `ErrorNode`s | Phases 2–4 |
| Comptime bool ordering (`true < false`) + wildcard `let _` patterns | `comptime { true < false }` rejects because `expect_int` rejects bool operands of ordering ops; `comptime { let _ = 1; ... }` rejects with "only simple `let <name>` patterns are supported". | Deliberate Phase-2 scope. Bool ordering has no obvious semantics (lexicographic? `false < true` like Rust's `bool` ordering?) — wait for a corpus motivation. Wildcard `let _` is straightforward (allocate a binding but never read it); add it when the corpus motivates. |
| Comptime over wider types (`comptime { true }`, `comptime { "..." }`, comptime over classes) | `CtValue::Int(i128)` is the only variant; bool / string / class inners reject | CT.3 sub-bundle: add CtValue arms as the corpus motivates them |
| `comptime fn foo() -> i32 { ... }` decl-level form | Not parsed as a comptime decoration; `comptime` is only an expression-position keyword (`parse_atom` handles it). **Deferred to Phase 5** per decision #4 above — when the runtime evaluator becomes a stack VM on MIR, `comptime fn` is a one-bit annotation on `MirFn` that the resolver consults at call sites. Building it on today's AST interpreter would fake fn-body inlining + parameter substitution, all thrown away at Phase 5. | Phase 5. **Workaround today**: module-level `let CONSTANT: T = comptime { ... };` (Phase 1 increment 11a's top-level statements) covers every shared-compile-time-constant use case without a callable form. |
| Multi-segment paths in expressions (`std::mem::Foo`) | Typeck `UNSUPPORTED_CONSTRUCT` | Phase 2 (modules imports) |
| Slice slicing (`s[1..3]`), array-to-slice coercion | No syntax / typing rules yet | Phase 2 |
| Pointer arithmetic, dereference (`*p`), address-of (`&x`) | No syntax / typing rules yet | Phase 3 with the memory model |
| Mixing `putchar` and implicit Print in the same program | Output ordering under piped stdout is `[all writes][all putchars]` because stdio buffers putchar but `write(2)` syscall bypasses stdio | Add an `extern fn fflush(stream: *u8) -> i32;` corpus pattern, OR document the rule (current state — see corpus design notes below) |
| `BinOp::Mod` and `BinOp::Pow` on float operands | Codegen falls through to `srem`/`urem` (wrong) or traps (Pow) | Typeck doesn't currently produce float `%` / `**`. If a future corpus does, add float arms in `lower_binop` (both backends now have a stub Unsupported / trap path) |
| `gw new` template parses cleanly | Templates use ``comptime {}`` syntax. The `comptime` keyword + integer-literal block is now accepted (CT.1), but the template's specific shape may still reject if it uses non-CT.1 inner expressions | Re-check the templates after CT.2 lands (arithmetic + control flow will widen the accepted inner shapes); rewrite to current syntax if still rejected |
| Windows CI coverage | `gw_codegen_llvm`'s `llvm-sys 180` dep can't be satisfied on Windows runners (no usable dev install path); Windows is dropped from the CI matrix | Either (a) feature-gate `gw_codegen_llvm` so Windows builds the rest of the workspace without it, or (b) find / build an llvm-sys-compatible LLVM 18 distribution for Windows. Until then, fmt / clippy / build / test all run only on Linux + macOS |
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
   — `gw build --backend=fast` (Cranelift, default) and
   `--backend=llvm` (LLVM 18 via inkwell) both compile the entire
   226-program corpus. Both consume the same `MirProgram`. LLVM is
   pinned to 18.x (inkwell 0.5 + `llvm-sys 180`); upgrading the
   feature flag in `[workspace.dependencies]` is a coordinated change
   to `gw_codegen_llvm/src/lib.rs` (intrinsic names + opaque-
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
    either `gw_codegen_fast::compile_program` or
    `gw_codegen_llvm::compile_program`. Both crates are
    unconditional workspace dependencies; there's no `cfg` gate on
    LLVM. Building the workspace requires LLVM 18 to be installed
    (see #25). Default is `fast` so `gw build foo.gw` keeps
    behaving as before. Naming reflects the crate names — `fast`
    survives the eventual TPDE swap inside `gw_codegen_fast`
    without a rename.
25. **LLVM 18 build prerequisites** (B.1) — the workspace needs
    `LLVM_SYS_180_PREFIX` set when invoking `cargo build` /
    `cargo test`. On macOS: `brew install llvm@18` and
    `export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18`. On Linux:
    install LLVM 18 dev libs from the official LLVM apt/yum repo
    (Ubuntu's bundled `llvm-dev` may be too old) and set
    `LLVM_SYS_180_PREFIX` to its prefix. Additionally, LLVM 18's
    system-libs (zstd / ffi / xml2 / curses) must be linker-findable;
    `gw_codegen_llvm/build.rs` adds `/opt/homebrew/lib` and
    `/usr/local/lib` on macOS so Homebrew's keg-only `zstd` etc.
    resolve without `RUSTFLAGS` rituals.
26. **LLVM aggregate ABI: plain `ptr`, no `sret`/`byval` attrs** (B.4)
    — `gw_codegen_llvm::make_fn_type` emits a hidden `ptr` for
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
    `gw build --backend=llvm` invocation creates a fresh
    `inkwell::context::Context`, builds the module, emits the object,
    drops the context. There's no cross-call context reuse. This is
    the reason the LLVM corpus test takes ~30s for 226 programs —
    LLVM target init dominates. Once we batch-compile in Phase 2 (a
    single `cargo build` of a multi-file project), share one context
    across the whole build invocation. For one-shot `gw build
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
    (O.1 lesson, expanded by O.3) — every site that switches on
    "aggregate vs primitive" must go through `is_aggregate_ty`,
    never inline `matches!(ty, Class | Slice)` checks. The O.1
    Cranelift bug was exactly this: two sites (local-allocation
    + `lower_assign_stmt` aggregate-dst branch) had out-of-sync
    inline patterns. The O.3 LLVM bug was a third (`make_fn_type`'s
    aggregate-return arm). All three sites now route through the
    helper; future aggregate variants (sum types, tagged unions,
    …) auto-fix.
39. **`?T` and `!T` are type-distinct, layout-identical** (O.3) —
    Phase 2 reuses the same `optional_layout` formula and the same
    `OptInner` enum for both `Ty::Optional(_)` and
    `Ty::ErrorUnion(_)`. `ty_assignable` adds `T → !T` parallel to
    `T → ?T` but never permits `?T ↔ !T` exchange in either
    direction; the source-level distinction enables future
    propagation operators (`try`, `catch`) to attach to `!T` only,
    while `??` and `nil`-pattern attach to `?T` only.
40. **`wrap_to_optional_if_needed` is the canonical wrap helper**
    (O.3 lesson) — every site that switches on T-vs-aggregate-
    tagged-T must go through this helper, never inline allocate
    + `AssignField` sequences. Three sites currently call it:
    `lower_let` (let-init coercion), `lower_return` (uses
    `LowerCx::fn_return_ty`), and `lower_call` (consults each
    callee param's type from `typed.sigs`). The O.3 retrospective
    found a fourth implicit site that didn't yet exist (no `match`
    arm bodies coerce to ?T/!T currently), but when one lands the
    same helper should fire. Mirrors decision #38: canonical
    predicate + canonical wrap.
41. **`Terminator::Unreachable` doubles as a trap** (O.3) — the
    `!`-assert err branch and any other "this point is
    unreachable, abort if reached" path use `Terminator::Unreachable`,
    which both backends lower as a hardware trap (Cranelift
    `fb.ins().trap(...)` with a user trap code; LLVM `unreachable`
    IR which the backend emits as `ud2` / equivalent). Phase 2 has
    no panic-message infrastructure yet; later sub-bundles
    (perhaps a `panic` runtime hook, or the Phase 3 borrow-check
    machinery) can attach metadata to the trap.
42. **Multi-file builds: auto-discover sibling `.gw` files** (F.1)
    — the driver enumerates the build target's parent directory
    for `.gw` files, sorts by path (deterministic def order
    regardless of `read_dir`'s OS-dependent traversal), reads each
    into one shared `SourceMap`, parses each into a
    `SyntaxNode<'bump>` (one `FileArena` per file, all sharing
    one `Bump`), and folds parse diagnostics into the build's
    primary bag via `DiagBag::merge`. The output executable still
    uses the build target's stem; sibling files contribute
    symbols but don't influence the output name. Manifest-driven
    builds (`build.gw`) remain a separate path.
43. **`mod` + `use` is opt-in, with per-file scoping** (F.2 /
    F.3) — files without a `mod` declaration go into a flat
    global pool (preserving F.1's module-free behaviour). A file
    with `mod foo;` puts its items in `module_tables[foo]`,
    addressable only via `use foo;`. Each file's effective
    scope = flat pool + own items + items from modules the file
    `use`s; `use foo;` in main.gw does NOT make foo's items
    visible to lib.gw (per-file scoping). `lookup_in_file(file,
    name)` is the canonical typeck entry point; the legacy
    `lookup(name)` falls back to the flat pool for AST-test
    callers without a file context. Multi-segment paths (`use
    foo::bar;`, `foo::bar()` at call sites) ride later widenings.
    The single canonical `lookup_in_file` predicate is the F.x
    analog of decision #38's `is_aggregate_ty` — every typeck
    name-lookup site goes through it, so future scoping
    refinements (lexical scoping in nested blocks, generic
    instantiation contexts, …) can extend it in one place.
44. **Comptime hookpoint is typeck-side, on the typed AST** (CT.1)
    — handoff open question #1 is resolved as option (a): typeck
    calls `gw_comptime::eval_comptime_block` directly from
    `synth_comptime` and stashes the resulting `CtValue` in
    `TypedModule::comptime_values: FxHashMap<NodePtr, CtValue>`.
    MIR's `lower_comptime` reads the stash and emits
    `Operand::Const` directly, never lowering the comptime block's
    body. The `gw_comptime` crate depends only on `gw_ast` +
    `gw_lex`. Per architecture Part B.11 / E.1 the Phase-5
    replacement is a stack VM operating on MIR; the on-disk
    semantics (`CtValue` enum, sandbox budgets, error variants)
    carry over so swapping the evaluator implementation is a
    contained change. The alternative (post-MIR fold pass) was
    deliberately rejected for CT.1: it would have required
    invoking the evaluator on a partially-lowered IR with
    aggregate state (locals, classes) that the CT.1 evaluator
    doesn't yet support, and the typeck-side path lets the
    materialised constant participate in bidirectional literal
    narrowing on the surrounding context.
45. **`CtValue` is a closed enum, currently `Int(i128)` +
    `Bool(bool)`** (CT.1 + CT.2b) — the evaluator's value type
    mirrors `Const` (which has `Int`, `Bool`, `Float`, `Unit`,
    `DataAddr`, `CStrAddr`, `Error`) but starts minimal. `Bool`
    landed in CT.2b alongside the four ordering ops and
    overloaded equality; wider arms (`Float`, aggregates) ride
    CT.3+ as the corpus motivates. The `i128` width is
    deliberately wider than any runtime integer type GW supports:
    comptime arithmetic in CT.2a can overflow intermediate values
    without losing precision (the `i128::checked_*` ops raise
    `EvalError::IntegerOverflow` when the *evaluator-level*
    representation overflows), and the final narrowing to the
    runtime `IntTy` happens at materialisation via `Const::Int
    { value, ty }` in `lower_comptime` (a separate concern, see
    decision #48). `Bool` has no width — the materialisation arm
    is `(CtValue::Bool(b), Ty::Bool) → Const::Bool(b)`, a direct
    1:1 mapping into the runtime `Const::Bool` already validated
    across both backends since Phase 1 increment 6.
46. **Parser tail-expression form widens uniformly, but only
    `parse_expr_stmt` consumes it** (CT.1) — `parse_stmt:538`'s
    block-like arm (`KwIf | KwWhile | KwFor | LBrace`) is
    *unchanged*; those forms still produce `ExprStmt`. The
    widening fires only when `parse_expr_stmt` parses an
    expression and the next token is `}` without an intervening
    `;`. Top-level statements close at `Eof`, not `}`, so
    module-level behaviour is unchanged. The `Block::tail_expr`
    accessor predated CT.1 but was unreachable; CT.1 makes it
    reachable for the first time. `synth_block` and `lower_block`
    already consumed `tail_expr` correctly, so the only sites
    that needed CT.1-specific code are `synth_comptime` (which
    runs the evaluator) and `check_fn_body` (which adds the
    E0315 guard rail — see #47).
47. ~~**E0315 `TAIL_EXPR_IN_FN_BODY` guards the latent
    implicit-tail-return shape** (CT.1)~~ — **RESOLVED in
    implicit-tail-return (commit `579c4f0`).** `check_fn_body`
    now calls `check_expr(tail, sig.ret, &mut cx)` when
    `body.tail_expr()` is set; the bidirectional narrowing
    already running for `let` initialisers and `return`
    operands handles literal-width adoption. A non-matching
    tail type diagnoses as the ordinary `TYPE_MISMATCH`
    (E0300). The E0315 constant is retired (comment-only in
    `ec`). MIR's `lower_fn` captures the lowered tail operand
    and installs it as `Terminator::Return(tail_operand)`.
    The guard rail served its purpose during CT.1 (turned a
    latent runtime trap into a compile-time diagnostic) and
    is no longer needed now that the proper semantics are
    wired. **Scope note**: this resolution is for
    bare-Expr tails only — `parse_stmt`'s block-like-statement
    arm stays unchanged, so `if`/`while`/`for`/`{ … }` at
    block tail position still wraps in `ExprStmt` and doesn't
    populate `tail_expr`. The parser widening + divergent-tail
    handling (the "discard u0 tails when fn returns non-u0"
    rule that would let `25_if_else.gw`-style programs keep
    working under widened parsing) rides a separate future
    sub-bundle.
48. **Comptime arithmetic is arbitrary-precision at eval time,
    narrows at materialisation** (CT.2a) — the evaluator works
    in `i128` throughout via `i128::checked_{add,sub,mul,div,rem}`.
    Evaluator-level overflow (a `checked_*` returning `None`)
    raises `EvalError::IntegerOverflow(span)`; the typeck side
    routes the failure through the existing E0314
    `COMPTIME_EVAL_FAILED` diagnostic with a user-facing message
    naming "comptime arithmetic overflowed `i128` during
    evaluation". The narrowing question — what happens when the
    final `i128` result doesn't fit the surrounding runtime
    `IntTy` (e.g. `let x: u8 = comptime { 256 };`) — is
    *deliberately separate* from the eval-time path; it lives
    in `gw_typeck` / MIR's materialisation step and rides the
    same `try_narrow_literal` machinery as ordinary integer
    literals (decision #16). The split keeps the eval-time
    overflow story clean: a `comptime { 1_000_000_000 *
    1_000_000_000 * 1_000_000_000 }` that overflows `i128`
    diagnoses regardless of the target type, while a value that
    fits `i128` but not the runtime slot diagnoses against the
    literal-span using the existing bidirectional-narrowing
    error path. Division-by-zero is a separate
    `EvalError::DivisionByZero(span)` variant (not folded into
    `IntegerOverflow`) so the user-facing message names the
    actual failure mode. Both variants are span-carrying for
    diagnostic construction, consistent with the rest of the
    `EvalError` enum.
49. **`expect_int` is the canonical operand-type predicate
    inside the comptime evaluator** (CT.2b lesson) — every
    `eval_binary` arm that requires an integer operand
    (arithmetic: `+ - * / %`; ordering: `< <= > >=`) routes
    through `expect_int(v: CtValue, span: Span) -> Result<i128,
    EvalError>`; `Unary(Minus, …)` does the same. The helper
    returns `EvalError::Unsupported` with a fixed "this operator
    requires an integer operand, found `bool`" message, so
    every ill-typed bool-in-integer-context shape produces an
    identical diagnostic regardless of which arm encountered
    it. The pattern parallels decisions #38 (`is_aggregate_ty`
    for every aggregate-vs-primitive switch) and #40
    (`wrap_to_optional_if_needed` for every T-vs-Optional wrap
    site) — canonical predicates collapse the spread-across-arms
    decision into one tested helper, so later widenings (CT.2c's
    let-bindings reading from a locals env, CT.x's
    function-call argument coercion) only have to learn the
    helper, not duplicate the check. Equality is deliberately
    *not* routed through `expect_int` — it accepts matching
    `(Int, Int)` or `(Bool, Bool)` pairs and rejects mixed pairs
    explicitly inside the `EqEq` / `BangEq` arm itself, naming
    the type mismatch rather than the operand shape. Bool
    ordering (`true < false`) reaches its rejection by passing
    through `expect_int` and failing on the bool operand;
    logical `&&` / `||` reach their rejection through the
    evaluator's outer `_` arm without ever invoking
    `expect_int`. The three rejection paths produce three
    distinct user-facing messages, so the user sees *why* their
    expression didn't take.
50. **`gw_comptime` is decoupled from `gw_typeck` via the
    `BindingEnv` trait + `NodePtr` moved to `gw_ast::cst`**
    (CT.2c — resolves open question #5 and resolves the
    structural problem CT.2c surfaced). The naive solution to
    "the evaluator needs to resolve `let` patterns and path
    expressions to local indices" would be `gw_comptime` →
    depend on `gw_typeck` → read the `BindingId`-keyed maps
    directly. That forms a cycle: `gw_typeck` calls into
    `gw_comptime::eval_comptime_block`. Two coordinated
    structural moves break the cycle while keeping the type
    surface clean:
    (a) **`NodePtr` moved from `gw_typeck::lib` to
    `gw_ast::cst`.** `NodePtr<'a>(pub &'a SyntaxNode<'a>)` is
    fundamentally an AST primitive — pointer-equality of
    SyntaxNodes is part of the AST traversal vocabulary, not
    typeck-specific machinery. Moving it to `gw_ast` lets any
    downstream crate that needs to key into side-tables
    derived from the CST do so without a typeck dep. `gw_typeck`
    keeps a `pub use gw_ast::cst::NodePtr` re-export so
    existing import paths (`use gw_typeck::{..., NodePtr, ...}`
    in `gw_mir`) see no API change.
    (b) **`gw_comptime` introduces `BindingEnv<'a>` trait** with
    `lookup_pat(NodePtr<'a>) -> Option<u32>` and
    `lookup_path(NodePtr<'a>) -> Option<u32>`. The evaluator
    only needs the numeric `u32` index — the public field of
    typeck's `BindingId(pub u32)` newtype — so the trait
    surface is purely CST + Rust primitives. `gw_typeck`
    constructs a small `TypeckBindingEnv<'a, 'tm>` adapter
    that borrows its `pat_bindings` / `path_bindings` maps and
    exposes them through the trait (converting `BindingId.0`
    at the trait boundary). The adapter borrow is scoped so
    `synth_comptime` can still mutate `comptime_values.insert`
    after `eval_comptime_block` returns.
    (c) **`Vec<Option<CtValue>>` indexed by `BindingId.0 as
    usize`** is the locals env shape (open question #5 resolved
    as option (a)). Mirrors runtime MIR's `Local` indexing,
    O(1) lookup, no allocator churn, no rustc_hash dep on
    `gw_comptime`. The `Option<_>` slot models
    "uninitialised" defensively: typeck's name-resolution
    should make a use-before-`let` unreachable from well-typed
    programs, but the evaluator still reports a clear
    Unsupported diagnostic rather than silently returning a
    stale value if one ever slips through. `store_local`
    `resize`s the vec on first write so the binding-index →
    slot mapping is dense from position 0 regardless of which
    BindingId typeck allocated first. `NoBindings` (zero-sized,
    `lookup_pat` and `lookup_path` both return `None`) is the
    canonical resolver for unit tests and for evaluating
    shapes that have no let / path-to-local references.
    The trio scales cleanly: CT.2d's branches don't change the
    locals shape; CT.2e's lazy `&&` / `||` doesn't either;
    a hypothetical comptime-fn call (deferred to Phase 5 per
    decision #4) would need a frame stack on top of this
    vec, not a replacement.

---

<a name="after-phase-1"></a>
## After Phase 1 — what's next

The architecture's Phase-1 exit gate (200-program corpus) is met
**and** the Phase-1 follow-up "Option A" (class/slice ABI + `as` casts)
landed across A.1–A.4. The "Option B" Phase-13 LLVM backend then
shipped across B.1–B.5. The "Option C" Phase-2 entry has closed
its four pre-comptime sub-bundles: c-strings (C.1 + C.2 + cleanup
#1), match (M.1 + M.2 + M.3), optional / error-union (O.1 + O.2 +
O.3), and modules (F.1 + F.2 + F.3). **CT.1 closed the comptime
tracer (commit `018d4eb`)**: integer-literal `comptime { N }`
blocks evaluate at compile time through a typeck-side tree-walking
interpreter, lower as `Operand::Const` at the use site, and run
bit-exactly on both backends. **CT.2a closed comptime integer
arithmetic (commit `ce5ada5`)**: `+ - * / %` inside `comptime
{ ... }` evaluate over `i128` via `checked_*` ops, with
`IntegerOverflow` / `DivisionByZero` raised through the existing
E0314 diagnostic. **CT.2b closed comptime comparisons + booleans
(commit `d9f8064`)**: `CtValue::Bool(bool)` lands; the four
ordering ops (`< <= > >=`) and overloaded equality (`==` / `!=`
across both int and bool operands) flow through op-first dispatch
in `eval_binary`; `lower_comptime` gains its first new
materialisation arm since CT.1, `(CtValue::Bool, Ty::Bool) →
Const::Bool(b)`. **CT.2c closed comptime let-bindings + locals
env (commit `c0d4540`)**: `BindingEnv` trait abstracts CST ↔
binding-index lookup; `NodePtr` moves from `gw_typeck` to
`gw_ast::cst` to break the dep cycle (decision #50);
`EvalCx` carries a dense `Vec<Option<CtValue>>` indexed by
`BindingId.0` (decision Q5 ⇒ option (a)); `eval_comptime_block`
walks `Stmt::Let` statements and `eval_expr` gains an
`Expr::Path` arm reading from the locals env. **CT.2d closed
comptime `if`/`else` (commit `a03c361`)**: `eval_expr` gains
an `Expr::If` arm calling new `eval_if`; condition is pinned
to `CtValue::Bool` via new `expect_bool` helper (parallel to
`expect_int`); exactly one arm evaluates — the un-taken arm
is never visited, so any latent side effect inside it (a
`1 / 0`, an Unsupported op) stays inert. First comptime
sub-bundle where the evaluator's control flow shape diverges
from the typed AST's syntactic walk. **CT.2e closed comptime
short-circuit `&&` / `||` (commit `9d062d3`)** — and with it,
CT.2 as a whole: `eval_binary` intercepts `AmpAmp` /
`PipePipe` before its eager RHS eval and routes to
`eval_logical_short_circuit`, which evaluates LHS first and
RHS only when LHS doesn't determine the result. Mirrors
decision #15's runtime 3-block lowering. **Implicit-tail-
return for bare-Expr tails closed (commit `579c4f0`)**:
typeck's `check_fn_body` now calls `check_expr(tail, sig.ret,
&mut cx)` when `body.tail_expr()` is set (bidirectional
narrowing handles literal-width adoption); MIR's `lower_fn`
wires the lowered tail operand into `Terminator::Return`. The
CT.1 E0315 guard rail is retired; a non-matching tail type
diagnoses as the ordinary TYPE_MISMATCH (E0300). Scope
deliberately limited to bare-Expr tails — the parser
widening for block-like statements (with divergent-tail
handling for `25_if_else.gw`-style programs) rides a future
sub-bundle. The remaining Phase-2 comptime work is CT.3
(wider types). The decl-level `comptime fn foo() -> i32 {
... }` form is resolved as deferred to Phase 5 (see resolved
open question #4 below).

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
- `trait` package manager (workspace's `gw_pkg` stub).
- modules module imports (`use std::mem::Foo`).
- `comptime` evaluator (workspace's now-active `gw_comptime`
  crate — CT.1 closed; CT.2+ pending).
- `match` expressions — **M.1 + M.2 + M.3 DONE** (commits
  `183e5b8` + `7d9c04d` + `2d85e65`). `match` accepts integer-
  literal, bool-literal, inclusive-range (`lo..=hi`), or-pattern
  (`a | b | c`), and wildcard forms; the MIR-side
  `lower_pattern_test` helper recurses for or-patterns and emits
  short-circuit pairs for ranges; codegen unchanged.
- error unions `!T`, optional types `?T` — **O.1 + O.2 + O.3 DONE**
  (commits `7c46d5b` + `c555777` + `5282bc8`). `?T` and `!T` as
  parallel 2-field aggregates {tag, payload} for primitive inners;
  `nil` literal adopts `?T`; bare `T` coerces to `?T` and `!T`;
  `??` coalesce operator on `?T`; `expr!` assert on `!T`;
  `match opt { nil => ..., _ => ... }` over `?T` scrutinees.
  Three bugs caught and fixed across the sub-bundle (O.1: 1, O.3:
  2; O.2: 0); all three were latent multi-site predicate-canonical
  -isation bugs of the form "every site that does X must go through
  one canonical helper" — both `is_aggregate_ty` (decision #38)
  and `wrap_to_optional_if_needed` (decision #40) now have three
  call sites each.
- modules imports — **F.1 + F.2 + F.3 DONE**
  (commits `57b275d` + `6969f64` + `aab3f0b`). Multi-file builds
  via auto-discovery in the build target's parent directory
  (decision #42); opt-in `mod <name>;` module declarations
  with single-segment `use <name>;` imports; per-file `use`
  scoping (decision #43). All four multi-file corpus projects
  pass on both backends. Zero bugs across the entire sub-bundle
  — organisational sub-bundles that don't introduce value-level
  novelty stay clean.
- `c"..."` C-string literals — **C.1 + C.2 DONE** (commits
  `1e8752c` + `bd3cf5d`). `c"..."` types as `[*:0]u8`, lowers via
  `Const::CStrAddr` to a parallel `__gw_cstr_<i>` rodata pass in
  both backends, decays to `*u8` at extern call sites; non-extern
  fn signatures accept `[*:0]u8` directly.
- comptime expression-level — **CT.1 + CT.2a + CT.2b + CT.2c
  DONE** (commits `018d4eb` + `ce5ada5` + `d9f8064` +
  `c0d4540`). CT.1: `comptime { N }` where N reduces to an
  integer literal through paren/unary-minus/nested-block
  evaluates at compile time. The evaluator is a tree-walking
  interpreter on the typed AST (decision #44), `CtValue`
  starts at `Int(i128)` (decision #45), and the parser change
  that exposes `Block::tail_expr` to consumers comes with an
  E0315 guard rail blocking the latent `fn { tail }` shape
  (decisions #46 + #47). CT.2a: the evaluator gains binary
  arithmetic (`+ - * / %`) over `i128` via `checked_*` ops,
  with `EvalError::IntegerOverflow` /
  `EvalError::DivisionByZero` raised for the two failure
  modes (decision #48). Materialisation-time narrowing (`i128
  → IntTy`) stays on the existing `try_narrow_literal` path.
  CT.2b: `CtValue` gains `Bool(bool)` (decision #45);
  `eval_binary` reorganises around op-first dispatch with
  three groups (arithmetic, integer ordering, overloaded
  equality); the canonical `expect_int` predicate routes every
  integer-required arm through one site (decision #49);
  `lower_comptime` gains the `(CtValue::Bool, Ty::Bool) →
  Const::Bool(b)` arm — the first new materialisation arm
  since CT.1. CT.2c: `EvalCx` carries a dense
  `Vec<Option<CtValue>>` locals env indexed by `BindingId.0`
  and a `&dyn BindingEnv<'a>` resolver (Q5 ⇒ option (a),
  decision #50); `eval_comptime_block` walks `Stmt::Let`
  statements; `eval_expr` gains an `Expr::Path` arm.
  `NodePtr` moves from `gw_typeck` to `gw_ast::cst` to
  break the dep cycle that would otherwise form between
  the evaluator and typeck. CT.2d: `eval_expr` gains an
  `Expr::If` arm calling new `eval_if`; new `expect_bool`
  helper parallels `expect_int` (decision #49). Exactly
  one arm evaluates — the first comptime sub-bundle where
  the evaluator's control flow diverges from the typed
  AST's syntactic walk. Else-if chains fall out for free
  (`else_branch` returns an `Expr` that re-enters
  `eval_expr`). CT.2e: `eval_binary` intercepts `AmpAmp`
  / `PipePipe` before its eager RHS eval and routes to the
  new `eval_logical_short_circuit` helper, which evaluates
  LHS first and RHS only when LHS doesn't determine the
  result (mirrors decision #15's runtime 3-block lowering
  compressed into one function). **Closes CT.2 entirely.**
  CT.3a (commit `0b3ccba`): `CtValue` gains `Float(f64)`;
  `eval_literal` recognises `FloatLit` via a new
  `parse_float_literal` helper; `eval_binary`'s arithmetic
  / ordering / equality arms refactored to dispatch on the
  operand-value tuple — admitting `(Int, Int)` and
  `(Float, Float)` pairs and rejecting mixed combinations
  explicitly. Float arithmetic uses Rust's IEEE-754 ops
  directly (`/` by `0.0` yields `±∞` / `NaN` with no
  `DivisionByZero` error; `NaN == NaN` is `false`; NaN
  ordering returns `false`). The CT.2b-era `expect_int`
  helper is removed (its sole consumers went away in the
  refactor); `expect_bool` stays. MIR's `lower_comptime`
  gains the `(CtValue::Float, Ty::Float) → Const::Float`
  materialisation arm with f32 / f64 width narrowing.
  Opens the CT.3 family. CT.3b (commit `f8bd7df`):
  `CtValue` gains `Str(Vec<u8>)` (owned-inline; `CtValue`
  loses `Copy` so the non-Copy payload works), `eval_literal`
  recognises `StringLit` via a new `decode_string_literal`
  helper kept in lockstep with `gw_mir`'s runtime decoder.
  No comptime ops on strings — strings are opaque payloads.
  MIR's `lower_comptime` signature gains a `&mut Builder`
  parameter; the new `(CtValue::Str, Ty::Slice(IntTy::U8))`
  arm materialises as the same `{data, len}` slice aggregate
  that runtime string literals build, sharing the rodata
  interning path via `lcx.string_literals`. Remaining CT.3
  widenings (classes, optionals, error unions) ride later
  CT.3 sub-bundles motivated by corpus need.

Estimated cost remaining: dozens of hours, distributed between
the remaining CT.3 widenings (classes, optionals, error unions)
and whatever path the `comptime fn` decl-level question takes.
Bug yield so far is **3 caught + 1 deferred-and-now-resolved**
across all twenty-one closed Phase-2 sub-bundles
(C.1+C.2+M.1+M.2+M.3+O.2+F.1+F.2+F.3+CT.1+CT.2a+CT.2b+CT.2c+CT.2d+CT.2e+impl-tail-ret+block-like-tail+CT.3a+CT.3b
= 0 caught, O.1 = 1 caught, O.3 = 2 caught, CT.1's E0315
"deferred" entry resolved by implicit-tail-return, against
a 12/A.x prediction of ~15-20 — the recombination
+ organisational sub-bundles under-shot prediction because
they reused already-validated value-level shapes; the
value-level-novel
sub-bundles (O.1 and O.3) hit prediction exactly; CT.1's bug
was *latent in a parser side-effect* rather than the
evaluator's value-level surface, refining the heuristic to
weight latent-shape risk independently). The block-like-tail
sub-bundle's prediction was ~1 (new divergent-tail policy);
observed yield was 0 because the parser change reused
`parse_expr_stmt`'s already-validated checkpoint shape, the
typeck rule had a tight non-overlapping pre-condition, and
the MIR mirror was structural. CT.3a's prediction was ~1-2
(new value-level path through `eval_binary`'s tuple dispatch,
new materialisation arm, NaN-handling subtleties at the
comparison ops); observed yield was 0 because Rust's f64
`<` / `<=` / `==` already implement IEEE-754 directly (the
evaluator is one delegation away from correct behaviour) and
the materialisation arm in MIR mirrors a runtime
`lower_literal` path validated against both backends since
Phase 1 increment 12. CT.3b's prediction was also ~1-2 (the
first non-Copy `CtValue` variant — broad API ripple; the
first `lower_comptime` arm that builds an aggregate rather
than emitting a single `Const`; a decoder duplication that
must stay in lockstep with the runtime); observed yield was
0 because the Copy → Clone ripple is compile-time-caught,
the materialisation directly mirrors `lower_string_literal`
which has been validated since increment 11b, and the
decoder duplication has a dedicated unit test
(`decode_string_literal_matches_runtime`) that pins the
contract explicitly. The remaining CT.3 widenings (classes,
optionals, error unions) introduce richer aggregate shapes
that the locals env's `Vec<Option<CtValue>>` doesn't yet
compose with naturally — sub-field access concerns weren't
exercised by CT.3b (strings are opaque payloads), so the
prediction stays armed for CT.3c.

The dual-backend test now in place means any Phase 2 codegen change
is automatically validated against both Cranelift and LLVM; this
became useful immediately on C.1 and stayed useful through M.3.

#### Remaining Phase-2 sub-bundles: CT.3c+, possibly `comptime fn`

**CT.2 is closed entirely**: CT.1 the comptime tracer (commit
`018d4eb`); CT.2a integer arithmetic (commit `ce5ada5`); CT.2b
comparisons + `CtValue::Bool` (commit `d9f8064`); CT.2c
let-bindings + locals env (commit `c0d4540`); CT.2d
`if`/`else` branch-eval discipline (commit `a03c361`); CT.2e
short-circuit `&&` / `||` (commit `9d062d3`). **Implicit-
tail-return for bare-Expr tails is closed** (commit
`579c4f0`): the canonical `fn add(a, b) -> i32 { a + b }`
shape compiles cleanly; E0315 is retired. **Block-like-tail
parser widening + divergent-tail discard is closed** (commit
`9ac51a1`): `fn classify(x: i32) -> i32 { if x < 0 { 1 }
else { 2 } }` works end-to-end; the `25_if_else.gw` /
`27_else_if.gw` / `163_print_padding.gw` divergent-tail
shapes are preserved by the u0-discard rule; CT.2d's
comptime paren-wrap workaround is retired. **CT.3a comptime
float is closed** (commit `0b3ccba`):
`comptime { 3.14 + 0.5 }`, float ordering / equality, and
the IEEE-754 contract (NaN comparisons return false,
division by 0.0 yields ±∞ without raising) all evaluate
bit-exactly on both backends; `CtValue::Float(f64)` joins
`Int` and `Bool`; MIR's `Const::Float` materialisation arm
narrows to f32 when the surrounding `Ty::Float` is `F32`.
**CT.3b comptime string literals is closed** (commit
`f8bd7df`): `comptime { "hello" }` evaluates to a
`CtValue::Str(Vec<u8>)` (the variant that cost `CtValue`
its `Copy` impl); MIR materialises as the same `{data,
len}` `[]u8` slice aggregate that runtime string literals
build, sharing the rodata interning path via
`lcx.string_literals`. No comptime ops on strings (concat,
`==`, `.len`) yet — ride a future sub-bundle if motivated.
What's left:

- **CT.3c+ comptime over richer aggregates** — classes,
  optionals, error unions as the corpus motivates. The
  `CtValue` enum gains the remaining shapes (now that
  `CtValue: !Copy` after CT.3b, aggregate variants like
  `CtValue::Class(Vec<CtValue>)` are admissible). Aggregate
  shapes introduce field-access concerns to the locals env
  — the current dense `Vec<Option<CtValue>>` indexing-by-
  `BindingId.0` doesn't compose naturally with sub-field
  reads. Optionals / error unions are parallel to the
  runtime `{tag, payload}` layout but the payload would be
  another `CtValue`, suggesting a recursive shape. Bug
  yield estimate: ~1-2 per shape introduction (aggregates:
  locals-env storage shape for sub-field access; optionals:
  payload-vs-tag materialisation). Suggested ordering:
  motivated by corpus need rather than speculative
  widening — pick the shape the next non-trivial comptime
  use case actually asks for. Also a candidate: **CT.3b'
  comptime string ops** (concat, `==`, `.len`) if a corpus
  program needs to compute strings rather than just paste
  literals.

Suggested ordering: CT.3c+ when a corpus program motivates
a new `CtValue` variant. `comptime fn` decl-level form is *not*
in Phase 2 scope — it rides Phase 5 alongside the stack-VM
evaluator (see resolved open question #4 below; shared
compile-time constants in Phase 2 use module-level
`let CONSTANT: T = comptime { ... };` instead).

Open questions to resolve at session start:

1. ~~**Where does compile-time evaluation hook into the
   pipeline?**~~ **RESOLVED in CT.1** (decision #44): typeck-side,
   on the typed AST. The evaluator runs from `synth_comptime` and
   stashes the `CtValue` in `TypedModule::comptime_values`; MIR
   reads the stash and emits a `Const` directly.
2. ~~**What does the evaluator's value type look like?**~~
   **RESOLVED in CT.1** (decision #45): `CtValue::Int(i128)` is
   the only variant today; CT.3 adds Bool / Float / aggregate
   arms as motivated by the corpus.
3. ~~**How does the evaluator handle errors / divergence?**~~
   **RESOLVED in CT.1**: `EvalError = { Unsupported{span, what},
   BudgetExceeded(span), StackOverflow(span), BadIntLiteral(span)
   }`. All variants carry spans for diagnostic construction.
   The `Budget` struct (10⁹ steps, 1024 depth) matches
   architecture E.3 caps and is threaded through the evaluator
   via `step()` / `enter()` / `exit()` helpers so CT.2's
   interpreter loop only has to call the existing primitives.
4. ~~**Does `comptime fn foo() -> i32 { ... }` ship in Phase
   2?**~~ **RESOLVED post-CT.1 (Path A — block-only in Phase
   2; `comptime fn` rides Phase 5).** Reasoning: (a) no
   expressiveness loss — module-level `let CONSTANT: T =
   comptime { ... };` (Phase 1 increment 11a's top-level
   statements) covers every use case `comptime fn` would; the
   compile-time-constant story is via constants, not via fn
   calls. (b) Phase 5 replaces the AST interpreter with a stack
   VM on MIR (architecture Part B.11); `comptime fn` is a
   one-bit annotation on `MirFn` there. Building `comptime fn`
   on today's AST interpreter would fake fn-body inlining and
   parameter substitution — machinery thrown out the moment
   Phase 5 lands. (c) The alternative (call-site desugar to a
   synthetic `ComptimeExpr`) would introduce a second
   provenance for the same CST shape, breaking the
   one-source-one-CST invariant for `gw dump` / `gw fmt` / LSP.
   The Phase-2 cost of deferral is purely ergonomic: shared
   compile-time constants go through module-level `let`
   bindings, not through callable comptime fns. Rust shipped
   without `const fn` for years; `const NAME: T = ...;`
   carried the load. GW takes the same deal.
5. ~~**Interpreter state model for CT.2c.**~~ **RESOLVED in
   CT.2c** as option (a): `Vec<Option<CtValue>>` indexed by
   `BindingId.0 as usize`. See decision #50 for the
   reasoning. The `Option<_>` slot models "uninitialised"
   defensively; the resize-on-first-write keeps the
   binding-index → slot mapping dense regardless of which
   BindingId typeck allocated first.

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
2. **Fix `gw new` templates** so the generated `hello.gw` parses
   under Phase-1 syntax. The bare-string-literal half already works
   after 11c; the `comptime` directive is still rejected. Easiest
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
   `gw_codegen_llvm` so Windows can `cargo build --workspace`
   without it (the more honest fix; touches `gw_driver`'s
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
# expect tip: HANDOFF refresh after CT.3b (this commit),
#             f8bd7df (CT.3b comptime string literals),
#             1df3f3b (HANDOFF refresh after CT.3a), 0b3ccba
#             (CT.3a comptime float arithmetic + comparisons),
#             2d069a6 (HANDOFF refresh after block-like-tail),
#             9ac51a1 (block-like-tail parser widening +
#             divergent-tail discard), c215ce4 (HANDOFF refresh
#             after impl-tail-ret), 579c4f0 (implicit-tail-return
#             for bare-expression tails), b8d8cc5 (HANDOFF
#             refresh after CT.2e), 9d062d3 (CT.2e comptime
#             short-circuit && / ||) at the bottom of head -10.

git status
# expect: clean working tree.

# LLVM 18 must be installed and discoverable for the workspace to build.
# On macOS:
which /opt/homebrew/opt/llvm@18/bin/llvm-config && /opt/homebrew/opt/llvm@18/bin/llvm-config --version
# expect: "18.x.x" (any 18.x — `brew install llvm@18` installs the
# current bottle, currently 18.1.8). If absent, `brew install llvm@18`
# and `brew install zstd` (LLVM 18's bottle links zstd).

export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18

. "$HOME/.cargo/env"
cargo test --manifest-path compiler/gw-bootstrap/Cargo.toml --workspace --no-fail-fast 2>&1 | grep "test result" | awk '{p+=$4;f+=$6}END{print p,f}'
# expect: "292 0"

ls tests/corpus/pass/phase1/*.gw | wc -l
# expect: 248 (was 245; +3 block-like-tail fixtures numbered
#         204_..206_ on top of the 4 implicit-tail-return
#         fixtures 200_..203_)

ls tests/corpus/pass/phase2_comptime/*.gw | wc -l
# expect: 31 (ct1_tracer + 4 ct2a_* + 4 ct2b_* + 4 ct2c_* + 5
#         ct2d_* + 5 ct2e_* + ct2d_if_no_paren + 5 ct3a_* +
#         2 ct3b_*)

ls -d tests/corpus/pass/phase2_multifile/*/ | wc -l
# expect: 4 (multi-file projects: add_two_files, cross_file_class,
#            mod_use, use_per_file)

ls tests/corpus/pass/lexparse/*.gw | wc -l
# expect: 62 (CT.1 added 062_comptime_tail_expr.gw)

ls compiler/gw-bootstrap/crates/ | wc -l
# expect: 17
```

If any of those fail, **don't start the next session's work** —
investigate first. The most likely culprits are stale `target/`
directories, an outdated `Cargo.lock`, or someone else's commits
between sessions. If `cargo build` fails inside `gw_codegen_llvm`
or `llvm-sys`, double-check `LLVM_SYS_180_PREFIX` and make sure the
LLVM 18 install hasn't been replaced by a newer version (the pin is
to 18.x specifically; 19+ won't work without bumping the inkwell
feature flag in `[workspace.dependencies]`).

---

## Appendix — useful commands

```bash
# Build the compiler (needs LLVM_SYS_180_PREFIX in the env)
export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18
cargo build --manifest-path compiler/gw-bootstrap/Cargo.toml -p gw_driver

# Compile and run a single .gw file (Cranelift, default)
GW=compiler/gw-bootstrap/target/debug/gw
$GW build path/to/file.gw
./path/to/file
echo $?

# Same file through the LLVM backend
$GW build --backend=llvm path/to/file.gw
./path/to/file
echo $?

# Inspect the AST for a file (Phase 0 dump)
$GW dump path/to/file.gw

# Disassemble a compiled binary (helpful for codegen bugs)
otool -tv path/to/binary  # macOS
objdump -d path/to/binary # Linux

# Run only the Phase-1 run-corpus through Cranelift
cargo test --manifest-path compiler/gw-bootstrap/Cargo.toml \
    -p gw_driver --test phase1_run

# Run the same corpus through the LLVM backend
cargo test --manifest-path compiler/gw-bootstrap/Cargo.toml \
    -p gw_driver --test llvm_backend

# Run the Phase-2 multi-file corpus (both backends)
cargo test --manifest-path compiler/gw-bootstrap/Cargo.toml \
    -p gw_driver --test phase2_multifile

# Run the Phase-2 comptime corpus (both backends, CT.x sub-bundles)
cargo test --manifest-path compiler/gw-bootstrap/Cargo.toml \
    -p gw_driver --test phase2_comptime

# Run the comptime evaluator's unit tests in isolation
cargo test --manifest-path compiler/gw-bootstrap/Cargo.toml \
    -p gw_comptime

# Run just the lex+parse snapshot corpus
cargo test --manifest-path compiler/gw-bootstrap/Cargo.toml \
    -p gw_parse --test corpus

# Update insta snapshots after intentional changes
INSTA_UPDATE=always cargo test \
    --manifest-path compiler/gw-bootstrap/Cargo.toml \
    -p gw_parse --test corpus

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
shape was already validated by 12b). The O.1 + O.2 + O.3 sub-bundle extended the same arrows yet
again: parser added `??` (16, 15) right-assoc and postfix `!`
+ `!T` type syntax; typeck added `Ty::Optional(OptInner)` and
`Ty::ErrorUnion(OptInner)` with closed inner enum, plus
`T → ?T` / `T → !T` coercion edges, `nil`-narrowing, and
`?T` / `!T` literal-pattern dispatch; MIR added
`let_ty_from_ast`, the canonical `wrap_to_optional_if_needed`
helper firing at three sites (`lower_let`, `lower_return`,
`lower_call`), `lower_nil_literal`, `lower_coalesce`,
`lower_must`, and the Optional-scrutinee branch in
`lower_pattern_test`; both backends added `Ty::Optional(_)` /
`Ty::ErrorUnion(_)` arms to `is_aggregate_ty` /
`aggregate_layout` / `aggregate_field_ty` (via the shared
`optional_layout` formula), and LLVM's `make_fn_type`
aggregate-return arm now routes through `is_aggregate_ty`. The
F.1 + F.2 + F.3 sub-bundle extended the resolver / driver /
typeck arrows: driver added auto-discovery + `Bump`-shared
multi-file parsing; resolver added `resolve_modules`,
`process_module`, per-module tables, per-file scopes, and
`lookup_in_file`; typeck added `Cx::current_file` and
`lookup_in_file`-routed name resolution. MIR / codegen
unchanged across the entire modules sub-bundle. The CT.1
sub-bundle adds a *vertical* branch to the diagram: between
typeck and MIR, the new `gw_comptime` crate becomes a sibling
arrow. When typeck encounters a `comptime { ... }` block it
detours through `gw_comptime::eval_comptime_block` (a tree-
walking interpreter on the typed AST), stashes the resulting
`CtValue` in `TypedModule::comptime_values`, and MIR's
`lower_comptime` reads the stash to emit `Const::Int` directly
— the comptime block's body never reaches MIR or codegen. The
parser also widens: `parse_expr_stmt` now leaves a bare `Expr`
child at block tails (no `;` before `}`), populating the
previously-unreachable `Block::tail_expr` accessor; typeck
guards the latent `fn { tail }` shape with E0315 until
implicit-tail-return ships as its own sub-bundle. CT.2a
thickens that vertical branch without touching the diagram:
`eval_binary` lands inside `gw_comptime` with `i128::checked_*`
arithmetic and the two new `EvalError` variants
(`IntegerOverflow`, `DivisionByZero`); the typeck/MIR boundary
sees the same `CtValue::Int(i128) → Const::Int { value, ty }`
shape it saw in CT.1, so MIR and codegen gain zero new arms.
CT.2b lands the first new shape at that boundary since CT.1:
`CtValue` gains `Bool(bool)`, `eval_binary` reorganises around
op-first dispatch with three groups (arithmetic, integer
ordering, overloaded equality) all routed through the canonical
`expect_int` operand-type predicate, typeck's `synth_comptime`
gate widens to `Ty::Int(_) | Ty::Bool`, and `lower_comptime`
gains the `(CtValue::Bool, Ty::Bool) → Const::Bool(b)` arm.
Codegen still gains zero new arms — the runtime `Const::Bool`
lowering has been validated since Phase 1 increment 6, so the
new materialisation reuses an already-known runtime shape.
CT.2c gives the evaluator real state without changing any arrow
in the diagram: `EvalCx` gains a dense `Vec<Option<CtValue>>`
locals env indexed by `BindingId.0` and a `&dyn BindingEnv<'a>`
resolver constructed by typeck over its `pat_bindings` /
`path_bindings` maps; `eval_comptime_block` walks `Stmt::Let`
statements and `eval_expr` gains an `Expr::Path` arm reading
from locals. The supporting structural move is upstream of
the diagram — `NodePtr<'a>` relocates from `gw_typeck::lib` to
`gw_ast::cst` so `gw_comptime` can key into typeck's
side-tables without forming a dep cycle. CT.2d gives the
evaluator branch-eval discipline without changing any arrow
either: `eval_expr` gains an `Expr::If` arm; the condition
evaluates via the new `expect_bool` helper (analog of
CT.2b's `expect_int`); exactly one arm runs and the un-taken
arm is never visited. The divergence between typeck's
walk-both-arms shape and the evaluator's walk-one-arm shape
is contained inside `eval_if` and has no typeck-side
counterpart — the first comptime feature whose correctness
the dual-backend test cannot independently verify. CT.2e
closes CT.2 by lifting CT.2d's branch-eval discipline up
to the operator level: `eval_binary` intercepts `AmpAmp` /
`PipePipe` before its eager RHS eval and routes to
`eval_logical_short_circuit`, which mirrors decision #15's
runtime 3-block lowering compressed into one Rust function
(LHS first, short-circuit if LHS is the operator's identity
element, otherwise eval RHS). Same "evaluator's control flow
diverges from the typed AST's syntactic walk" pattern as
CT.2d but at finer granularity. **Implicit-tail-return (commit `579c4f0`)** changes one arrow
in the diagram — the typeck → MIR edge for fn bodies — by
adding one new call site at each end: typeck's `check_fn_body`
now calls `check_expr(tail, sig.ret, …)` (the bidirectional
narrowing already wired for `let` initialisers and `return`
operands gets a third caller); MIR's `lower_fn` captures
`lower_block`'s returned operand and installs it as
`Terminator::Return(tail_operand)` when the body had a
bare-Expr tail. The literal-width plumbing is unchanged —
`lower_literal` already reads `typed.expr_types` for the
`IntTy`; typeck's new `check_expr(tail, sig.ret, …)` call
just populates a new entry in that map. The CT.1 E0315
guard rail is retired (no diagnostic to emit any more).
**Block-like-tail parser widening + divergent-tail discard
(commit `9ac51a1`)** then extends the same triad — the parser
checkpoint trick from `parse_expr_stmt` is grafted onto
`parse_stmt`'s `KwIf | KwWhile | KwFor | LBrace` arm so
block-like expressions at the end of a block become the
block's `tail_expr` rather than wrapping in `ExprStmt`;
typeck's `check_fn_body` adds a "discard u0 tails when fn
returns non-u0" rule (preserves `25_if_else.gw` /
`27_else_if.gw` / `163_print_padding.gw`); MIR's `lower_fn`
mirrors the discard via an `expr_types[NodePtr(tail)]`
lookup. One parser change unblocks two surfaces — fn-body
block-like tails AND CT.2d's comptime paren-wrap workaround.
**CT.3a (commit `0b3ccba`)** then opened the CT.3 family by
widening the evaluator from `(Int, Bool)` to `(Int, Bool,
Float)`. `eval_binary`'s arithmetic / ordering / equality arms
refactored to dispatch on the operand-value tuple — admitting
both `(Int, Int)` and `(Float, Float)` pairs and rejecting
mixed pairs explicitly so the user sees the type mismatch
rather than an arbitrary dominant-type rule. Float operations
use Rust's IEEE-754 ops directly: `+ - * / %` are total, `/`
and `%` by `0.0` yield `±∞` / `NaN` with **no
`DivisionByZero` error** (distinct from the integer path,
which still raises); ordering and equality return `false` for
any pair involving `NaN`. MIR's `lower_comptime` gains the
`(CtValue::Float, Ty::Float) → Const::Float` materialisation
arm with width-aware bit pattern (`f.to_bits()` for `F64`,
`(f as f32).to_bits() as u64` for `F32`), mirroring
`lower_literal`'s runtime `FloatLit` path. **CT.3b (commit
`f8bd7df`)** then added `CtValue::Str(Vec<u8>)` for string
literal bytes (owned-inline; `CtValue` lost its `Copy` impl
because `Vec<u8>` isn't `Copy`). `eval_literal` recognises
`SyntaxKind::StringLit` and decodes via a new
`decode_string_literal` helper kept byte-for-byte in
lockstep with `gw_mir::decode_string_literal`. MIR's
`lower_comptime` signature gains a `&mut Builder` parameter
(the call site in `lower_expr` already has one) so the new
`(CtValue::Str, Ty::Slice(IntTy::U8))` arm can build a
slice aggregate: intern bytes into `lcx.string_literals`,
allocate a fresh `Ty::Slice(IntTy::U8)` local, push
`AssignField data = Const::DataAddr(id)` +
`AssignField len = Const::Int(n, USize)`. The
materialisation shape is identical to runtime
`lower_string_literal`'s, sharing the rodata interning
path. No comptime ops on strings — strings are opaque
payloads. The 248-program phase1 corpus + 4 multi-file
projects + 31 phase2_comptime tracers (`ct1_tracer.gw` → 4; `ct2a_*` 4 tracers; `ct2b_*` 4
tracers; `ct2c_*` 4 tracers; `ct2d_*` 5 tracers including the
canonical branch-eval regression test
`ct2d_un_taken_safe.gw` → 5; `ct2e_*` 5 tracers including the
canonical short-circuit regression test
`ct2e_and_short_circuit.gw` → 0 where the un-evaluated RHS
contains `1 / 0` and would crash if evaluated;
`ct2d_if_no_paren.gw` → 7 retiring CT.2d's paren-wrap
workaround; 4 implicit-tail-return fixtures
`200_tail_return_arith.gw` → 7,
`201_tail_return_literal.gw` → 42,
`202_tail_return_let_then_path.gw` → 22,
`203_tail_return_widening.gw` → 100; 3 block-like-tail
fixtures `204_tail_return_if.gw` → 30,
`205_tail_divergent_if.gw` → 22,
`206_tail_while_u0.gw` → 0; 5 CT.3a fixtures
`ct3a_lt.gw` → 7, `ct3a_add_eq.gw` → 9,
`ct3a_negation.gw` → 5, `ct3a_div.gw` → 8, and the canonical
IEEE-754 NaN-ordering regression test `ct3a_nan_ordering.gw`
→ 7 where `0.0 / 0.0 < 1.0` returns false and takes the
`else` arm; and 2 CT.3b fixtures `ct3b_string_literal.gw`
→ exit 0 stdout "hi\n" and `ct3b_string_escape.gw` → exit 0
stdout "a\tb\n" pinning the runtime decoder lockstep on
multi-escape payloads) are the direct test surface for
every one of those arrows, exercised through both backends
in CI.
