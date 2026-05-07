# GW Bootstrap — Session Handoff

This document is the entry point for the next development session. Read it first.

> **Last updated:** after Phase 1 increment 12h (full increment 12 complete).
> **Repo root:** `/Users/silmaril/Documents/GitHub/gw`
> **Workspace test count:** 121 unit + integration tests, all green.
> **Corpus size:** 61 Phase-0 lex+parse snapshots + 200 Phase-1 run-tests.
> **Phase 1 exit gate met.** Corpus has hit the 200-program target.

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

**Phase 0 is complete. Phase 1 is functionally complete** — the
architecture's exit criterion ("200 small `.gw` programs that compile and
run") has been met. The remaining items are a follow-up — LLVM port (13) —
which was explicitly deferred when Cranelift replaced LLVM as the
Phase-1 backend, plus the gaps listed under [What doesn't work
yet](#what-doesnt-work-yet-phase-1-deferred-or-incomplete).

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
│   ├── pass/phase1/             (200 .gw + .expected_exit / .expected_stdout)
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
│       ├── arsenal_driver/      ★ active (the `arsenal` binary)
│       ├── arsenal_codegen_llvm/  stub  (Phase 13)
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

### Active crate roles (≈5 100 LoC of compiler logic)

| Crate | Phase | Role |
|---|---|---|
| `arsenal_lex` | 0 | UTF-8 lexer state machine. 108-variant `TokenKind`, phf keyword table, `Span`/`SourceMap`/`Diagnostic`/`DiagBag` types. |
| `arsenal_ast` | 0 | Hand-rolled rowan-style CST + typed AST. Single unified `SyntaxKind` enum (188 variants). Typed views for ~32 Phase-1 node kinds; `Stub` variants for the rest. `Module::stmts()` exposes top-level stmts (11a). Bumpalo arena per file. Pretty-printer for `arsenal dump`. |
| `arsenal_parse` | 0 | Recursive-descent + Pratt expression precedence. Error-recovering. Produces both CST and AST. No parser generator. `parse_module` forks on `peek_item_keyword` between item and stmt (11a). `parse_type` handles `*T` / `[]T` / `&T` / `?T` / `[N]T`. |
| `arsenal_resolve` | 1 | Walks the AST, registers top-level fn + class defs, exports `primitive_type_name()`. `DefKind::SyntheticMain` is registered when top-level stmts coexist without explicit `fn main` (11a). |
| `arsenal_typeck` | 1 | Bidirectional checker. `Ty` enum: `U0`/`Bool`/`Int(IntTy)`/`Float(FloatTy)`/`Rune`/`Class(DefId)`/`Slice(IntTy)`/`Ptr(IntTy)`/`Error`. Emits a `TypedModule` with per-CST-node `expr_types`, `path_bindings`, `pat_bindings`, `call_targets`, `sigs`, `classes`. Slice + raw-pointer surface (11b/11c) are FFI-restricted. **Bidirectional literal narrowing (12d/12h)**: `check_expr` calls `try_narrow_literal` first — bare `IntLit`/`FloatLit`, `Unary(Minus, Literal)`, and `Paren(...)` shapes adopt the expected width when the value fits; out-of-range diagnoses against the literal span. `synth_binop_operands` extends the same rule across binary operators so `n < 2` (with `n: i64`) types cleanly. |
| `arsenal_mir` | 1 | CFG of basic blocks; primitive locals + aggregate stack-slot locals (class + slice); `Assign`/`AssignField` statements; `Use`/`BinOp`/`UnOp`/`Field` rvalues; `Goto`/`Branch`/`Return`/`Call`/`Unreachable` terminators. Loop-target stack for break/continue. `lower_for` desugar. `Const::DataAddr` + program-level `string_literals` table for `.rodata` payloads (11b). Implicit Print at stmt-position string lits desugars to `write(1, slice.data, slice.len)`; auto-injects `extern fn write` if user didn't declare one (11c). **Short-circuit `&&` / `\|\|` (12b)**: `lower_short_circuit` emits a 3-block control-flow shape (rhs-eval / short-circuit / join) and bypasses `lower_binary` so the RHS is only evaluated when the LHS doesn't determine the result. `BinOp::LogAnd`/`LogOr` are no longer produced by lowering; the codegen arms remain as dead code for enum symmetry. |
| `arsenal_codegen_fast` | 1 | Cranelift-backed (placeholder until Phase 7 TPDE port). Aggregate (class + slice) layouts → stack slots; field reads/writes → stack_load/stack_store; aggregate-aggregate assigns → field-by-field copy. String literals materialised via `module.declare_data` + `define_data_object` under `__gw_str_<i>` symbols (11b). `*T` raw pointers lower as pointer-sized scalars (11c). **Float comparisons (12a)**: `lower_binop` branches on `ty.is_float()` for `Eq`/`Ne`/`Lt`/`Le`/`Gt`/`Ge` — floats use `fcmp` with the matching `FloatCC`, ints keep `icmp`. |
| `arsenal_driver` | 0/1 | Subcommands: `arsenal new <name>`, `arsenal build <file.gw>`, `arsenal dump <path>`, `arsenal --version`. Build pipeline: lex → parse → resolve → typeck → MIR → Cranelift → object → `cc` link → executable. |

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
primitives and produced zero. The pattern is reliable enough to use as a
risk heuristic when planning future bundles.

### What 200 corpus programs cover

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
$ arsenal dump path/to/some.gw     # AST dump for debugging
$ arsenal --version
arsenal 0.0.1
```

### Test infrastructure

- `cargo test` at workspace root runs the entire suite (121 tests).
- `cargo test -p arsenal_parse --test snake_eater` runs the lex+parse
  insta snapshot corpus (61 pass, 5 fail).
- `cargo test -p arsenal_driver --test phase1_run` runs every
  `tests/snake_eater/pass/phase1/*.gw` end-to-end: builds, executes,
  matches exit code (and stdout where `.expected_stdout` is present).
  Skipped on Windows (`#![cfg(not(windows))]`) — `cc` integration is a
  later concern.
- CI workflow at `.github/workflows/ci.yml` runs build + fmt --check +
  clippy `-D warnings` + test on Linux / macOS / Windows.

---

## What doesn't work yet (Phase-1-deferred or incomplete)

| Limitation | Surface | Path forward |
|---|---|---|
| Class-typed fn params and return values | Typeck rejects with `UNSUPPORTED_CONSTRUCT` | Phase 1 follow-up: by-pointer ABI in codegen + `Rvalue::Use(class_local)` in non-let contexts |
| Slice-typed fn params and return values | Typeck rejects with `UNSUPPORTED_CONSTRUCT` | Same by-pointer ABI work as classes; once it lands, slices flow naturally as 2-field aggregates |
| Raw pointers outside `extern fn` signatures | Typeck rejects `*T` in non-extern fn params/returns | Memory-model + borrow-checker work (Phase 3); also blocks meaningful pointer arithmetic |
| Nested class fields | Typeck rejects | Generalise size/offset computation in `resolve_class_layout`; recurse on `Ty::Class` field types |
| Slice-typed class fields | Typeck rejects | Class layout would need to embed the slice's `(data, len)` pair |
| `as` casts between numeric types | No syntax / typing rules yet | Phase 1 follow-up: trivial widening (`i32 as i64`) and same-bit-width unsigned↔signed reinterpretation. Currently absent — `let n: i64 = some_i32;` has no escape hatch |
| `match`, error unions (`!T`), generics, `cipher`, async, comptime | Parser produces `ErrorNode`s | Phases 2–4 |
| Multi-segment paths in expressions (`std::mem::Foo`) | Typeck `UNSUPPORTED_CONSTRUCT` | Phase 2 (frequencies / module imports) |
| `c"..."` C-string literals (`[*:0]u8`) | Typeck records `Ty::Error` | Phase 2 — sentinel-pointer machinery |
| Slice slicing (`s[1..3]`), array-to-slice coercion | No syntax / typing rules yet | Phase 2 |
| Pointer arithmetic, dereference (`*p`), address-of (`&x`) | No syntax / typing rules yet | Phase 3 with the memory model |
| Mixing `putchar` and implicit Print in the same program | Output ordering under piped stdout is `[all writes][all putchars]` because stdio buffers putchar but `write(2)` syscall bypasses stdio | Add an `extern fn fflush(stream: *u8) -> i32;` corpus pattern, OR document the rule (current state — see corpus design notes below) |
| Functions without explicit return type (`fn f(x: i32) {`) | Parser rejects with E0307 | Add a default `-> u0` arm to `parse_fn_decl` if the user wants to elide it. Currently every fn must declare its return type |
| `BinOp::Mod` and `BinOp::Pow` on float operands | Codegen falls through to `srem`/`urem` (wrong) or traps (Pow) | Typeck doesn't currently produce float `%` / `**`. If a future corpus does, add float arms in `lower_binop` |
| LLVM backend | `arsenal_codegen_llvm` stub only | **Increment 13** — not session-blocking |
| `arsenal new` template parses cleanly | Templates use `#virtuous {}` syntax that Phase 1 parser rejects | Swap templates to Phase-1 syntax (the bare-string-literal half now works after 11c, but the `#virtuous` directive is still rejected) |

### Corpus design notes (rules learned during increment 12)

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
3. **Class-typed fn parameters are still rejected** (typeck `E0306`).
   When designing a corpus program around a class state-machine helper,
   inline the per-step logic at every call site instead of factoring a
   `record(s: Stat, v: i32)` helper. The capstone (200) does this.
4. **No `as` cast between numeric widths.** A `let n: i64 = …;` value
   has no escape hatch back to `i32` for use in a function that expects
   `i32`, and vice versa. Workaround: parallel typed locals (one i32,
   one i64) advanced together inside a loop. Or commit to a single
   width across the program.
5. **Exit codes are 8-bit (POSIX).** Programs that compute a sum > 255
   and return it observe `result % 256` as the exit code. Either keep
   sums small or check the value via `if r == EXPECTED { return
   SOME_SMALL_I32; }` (the standard pattern across most of the
   wide-int and float corpus).

---

## Known design decisions worth re-confirming next session

These are user-approved choices that affect ongoing work. Re-confirm at
session start before changing them.

1. **Tracer-bullet ordering**: each Phase-1 increment is end-to-end
   compileable + runnable, never "build subsystem N to completion then
   subsystem N+1". *(approved at start of Phase 1)*
2. **Cranelift backend now, LLVM port deferred to Phase 13** — explicit
   deviation from architecture Part F.2 which mandates LLVM for Phase 1.
   `arsenal_codegen_llvm` stub still pinned to `inkwell 0.5` /
   `llvm-sys 180` in workspace deps.
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
    suspicion, not by program count.

---

<a name="after-phase-1"></a>
## After Phase 1 — what's next

The architecture's Phase-1 exit gate (200-program corpus) is met.
Three independent next steps are all reasonable; pick based on
session goal, not order. They are roughly listed in order of
"unlocks the most subsequent work" first.

### Option A — Phase-1 follow-up: class/slice ABI + `as` casts

Corpus authoring during 12 ran into the same wall repeatedly: helpers
that take or return class / slice values can't be written, and there's
no `as` cast to bridge int widths. Both would extend Phase 1's
expressiveness without crossing into Phase 2's scope.

**ABI work** (typeck + MIR + codegen):
- Allow `Ty::Class(_)` and `Ty::Slice(_)` in fn parameter and return
  positions.
- Codegen: pass aggregates by hidden pointer (System V ABI's "memory
  class" rule for any struct > 16 bytes; for ≤ 16 bytes, the proper
  ABI is "split into two registers" but Phase 1 can punt to
  by-pointer-always to keep the code simple).
- MIR: `Rvalue::Use(class_local)` outside `let` initialisers (e.g. as
  a function argument or return expression).
- Typeck: drop the `UNSUPPORTED_CONSTRUCT` rejection in
  `check_fn_signature` for class / slice param/return types.

**`as` casts** (lex + parse + typeck + MIR + codegen):
- Lex: `as` is already a keyword (it's in the keyword phf table —
  verify).
- Parse: post-fix `expr as Type` at a Pratt precedence below `*`/`/`
  but above comparison.
- Typeck: define the legal cast matrix — int↔int (any width, signed
  ↔ unsigned reinterpret as bit cast), int↔float (sitofp / fptosi /
  uitofp / fptoui), float↔float (fpext / fptrunc), nothing else for
  Phase 1.
- MIR: new `Rvalue::Cast { kind, operand, ty }` with a finite `kind`
  enum (`IntWiden`, `IntTrunc`, `IntSign`, `FloatExt`, `FloatTrunc`,
  `IntToFloat`, `FloatToInt`).
- Codegen: each kind maps to one Cranelift op (`uextend`, `sextend`,
  `ireduce`, `fpromote`, `fdemote`, `fcvt_to_sint`, `fcvt_to_uint`,
  `sitofp`, `uitofp`).

Estimated cost: 6-12 hours. Bug yield: medium — sign-handling and
narrowing-vs-truncation are easy to get backwards.

### Option B — Phase 13: LLVM backend port

`arsenal_codegen_llvm` is currently a stub. The architecture's Part
F.2 originally mandated LLVM for Phase 1; we deviated to Cranelift
to ship faster. Phase 13 is reinstating LLVM as a parallel backend,
which then becomes the default for release builds.

Workspace deps already pin `inkwell 0.5` / `llvm-sys 180`. The MIR is
backend-agnostic; both Cranelift and LLVM consume the same
`MirProgram`. Port the existing 200-program corpus through a
`--backend=llvm` driver flag and ensure exit codes / stdout match.

Estimated cost: 15-30 hours. Bug yield: high — every backend
divergence (NaN handling, sign extension, calling convention quirks)
shows up as a divergence between the two backends.

### Option C — Phase 2: comptime + module system

The big jump. Phase 2 brings:
- `cipher` package manager (workspace's `arsenal_cipher` stub).
- `frequencies` module imports (`use std::mem::Foo`).
- `comptime` evaluator (workspace's `arsenal_comptime` stub).
- `match` expressions, error unions `!T`, optional types `?T`.
- `c"..."` C-string literals.

Estimated cost: dozens of hours. Bug yield: untyped — Phase 2 is
where the parser stops emitting `ErrorNode` for the spec's harder
features and where the runtime model gets complicated (compile-time
evaluation, module resolution, async).

### Tactical cleanup (any session)

These are all self-contained and worth landing whenever a session
runs short on time for the bigger items:

1. **Default `-> u0` for fn declarations.** One arm in `parse_fn_decl`.
2. **Fix `arsenal new` templates** so the generated `hello.gw` parses
   under Phase-1 syntax. The bare-string-literal half already works
   after 11c; the `#virtuous` directive is still rejected. Easiest
   fix: rewrite the templates to use today's syntax.
3. **`-> u0` returning `()` semicolons.** Confirm the corpus rules
   hold for fn bodies that fall through without a `return`.
4. **Float `Mod` and `Pow`** codegen arms (currently fall through to
   integer ops; harmless because typeck doesn't produce them, but
   not future-proof).

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
# expect tip: 12h (8bc26a4), then 12g (42e17cc), 12f (3d91072), 12e (3543601),
#             12d (aa1536d), 12c (6fb3d45), 12b (add7fe0), 12a (e45723d),
#             11c (0bf40f9), 11b (2545bb7) at the bottom of the head -10.

git status
# expect: clean working tree (no .DS_Store, no .probe leftovers)

. "$HOME/.cargo/env"
cargo test --manifest-path compiler/arsenal-boot/Cargo.toml --workspace 2>&1 | grep "test result" | awk '{p+=$4;f+=$6}END{print p,f}'
# expect: "121 0"

ls tests/snake_eater/pass/phase1/*.gw | wc -l
# expect: 200

ls compiler/arsenal-boot/crates/ | wc -l
# expect: 17
```

If any of those fail, **don't start the next session's work** —
investigate first. The most likely culprits are stale `target/`
directories, an outdated `Cargo.lock`, or someone else's commits
between sessions.

---

## Appendix — useful commands

```bash
# Build the compiler
cargo build --manifest-path compiler/arsenal-boot/Cargo.toml -p arsenal_driver

# Compile and run a single .gw file
ARSENAL=compiler/arsenal-boot/target/debug/arsenal
$ARSENAL build path/to/file.gw
./path/to/file
echo $?

# Inspect the AST for a file (Phase 0 dump)
$ARSENAL dump path/to/file.gw

# Disassemble a compiled binary (helpful for codegen bugs)
otool -tv path/to/binary  # macOS
objdump -d path/to/binary # Linux

# Run only the Phase-1 run-corpus
cargo test --manifest-path compiler/arsenal-boot/Cargo.toml \
    -p arsenal_driver --test phase1_run

# Run just the lex+parse snapshot corpus
cargo test --manifest-path compiler/arsenal-boot/Cargo.toml \
    -p arsenal_parse --test snake_eater

# Update insta snapshots after intentional changes
INSTA_UPDATE=always cargo test \
    --manifest-path compiler/arsenal-boot/Cargo.toml \
    -p arsenal_parse --test snake_eater
```

---

## One-line architecture summary

```
.gw → lex → parse → resolve → typeck → MIR → Cranelift → object → cc → executable
```

Each arrow is a separate active crate; each bug we've caught lived at exactly
one of those arrows. Increment 11 (a/b/c) extended the leftmost arrows
(parser accepts top-level stmts and `*T` raw pointers; `[]u8` slice type
with `.rodata` storage; implicit Print desugar at statement position).
Increment 12 didn't change the arrow topology either, but it forced four
small but real fixes that lived inside the existing arrows: `fcmp` dispatch
in codegen (12a), short-circuit control-flow lowering in MIR (12b),
bidirectional literal narrowing in typeck (12d/12h), and the regression
test suites that pin those fixes in place. The 200-program corpus is the
direct test surface for every one of those arrows.
