# GW Bootstrap — Session Handoff

This document is the entry point for the next development session. Read it first.

> **Last updated:** after commit `c7870bb` (Phase 1 increment 10).
> **Repo root:** `/Users/silmaril/Documents/GitHub/gw`
> **Workspace test count:** 95 unit + integration tests, all green.
> **Corpus size:** 61 Phase-0 lex+parse snapshots + 71 Phase-1 run-tests.

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
`for x in 0..n`, and `extern fn putchar` calls into libc all work and are
covered by the corpus.

**Phase 0 is complete. Phase 1 is roughly 75% complete** by the architecture's
own milestone list. Two increments remain before Phase 1's exit criterion is
in reach (11: top-level stmts + implicit Print, 12: 200-program corpus). A
third — LLVM port (13) — is explicitly deferrable and was scoped out of the
current backend (Cranelift took its place).

---

## Where to start the next session

Read this whole document, then in priority order:

1. **`docs/spec.md` §5.3** (lexical structure) — refresher only.
2. **`docs/architecture.md` Part L Phase 1 deliverables** — the contract.
3. **`docs/architecture.md` Part B.3, C.3, D.1, F.1** — pipeline shape.
4. **The most recent commit message** (`git log -1`) — picks up the thread.
5. **`tests/snake_eater/pass/phase1/`** — skim a few `.gw` files to see
   what currently compiles and runs.

Then jump to **[Next increment — 11](#next-increment--11)** below.

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
│   ├── pass/phase1/             (71 .gw + .expected_exit / .expected_stdout)
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

### Active crate roles (≈4 100 LoC of compiler logic)

| Crate | Phase | Role |
|---|---|---|
| `arsenal_lex` | 0 | UTF-8 lexer state machine. 108-variant `TokenKind`, phf keyword table, `Span`/`SourceMap`/`Diagnostic`/`DiagBag` types. |
| `arsenal_ast` | 0 | Hand-rolled rowan-style CST + typed AST. Single unified `SyntaxKind` enum (188 variants). Typed views for ~30 Phase-1 node kinds; `Stub` variants for the rest. Bumpalo arena per file. Pretty-printer for `arsenal dump`. |
| `arsenal_parse` | 0 | Recursive-descent + Pratt expression precedence. Error-recovering. Produces both CST and AST. No parser generator. |
| `arsenal_resolve` | 1 | Walks the AST, registers top-level fn + class defs, exports `primitive_type_name()`. |
| `arsenal_typeck` | 1 | Bidirectional checker. `Ty` enum: `U0`/`Bool`/`Int(IntTy)`/`Float(FloatTy)`/`Rune`/`Class(DefId)`/`Error`. Emits a `TypedModule` with per-CST-node `expr_types`, `path_bindings`, `pat_bindings`, `call_targets`, `sigs`, `classes`. |
| `arsenal_mir` | 1 | CFG of basic blocks; primitive locals + class stack-slot locals; `Assign`/`AssignField` statements; `Use`/`BinOp`/`UnOp`/`Field` rvalues; `Goto`/`Branch`/`Return`/`Call`/`Unreachable` terminators. Loop-target stack for break/continue. Tracker for desugaring `for x in lo..hi`. |
| `arsenal_codegen_fast` | 1 | Cranelift-backed (placeholder until Phase 7 TPDE port). Class layouts → stack slots; field reads/writes → stack_load/stack_store; class-class assigns → field-by-field copy. |
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

**Key pattern**: each "0 bugs" increment was almost pure corpus growth (the
plumbing was already in place). Each "≥1 bug" increment caught real
miscompiles before they could compound. The tracer-bullet ordering paid off
visibly — every bug caught was 1 commit's worth of debugging instead of N+
commits' worth of "why is this wrong?"

### What 71 corpus programs cover

- Phase-0 syntax: every TokenKind variant, every operator precedence
  level, every supported statement form.
- Phase-1 semantics: integer arithmetic + comparison + bitwise + shift
  + logical ops on signed and unsigned integers; bool literals + `!`;
  function declarations with up to 2 params and i32 return; recursive
  calls (fib, fact); `let` with explicit and inferred types; `if`,
  `if/else`, `else if`, `while`, `break`, `continue`, `for x in 0..n`,
  `for x in 0..=n`, nested loops; assignment expressions; `extern fn`
  + stdout-comparison via libc `putchar`; classes with up to 3
  fields, field read, field write, class fields driving control flow.

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

- `cargo test` at workspace root runs the entire suite (95 tests).
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
| Nested class fields | Typeck rejects | Generalise size/offset computation in `resolve_class_layout`; recurse on `Ty::Class` field types |
| Integer literal narrowing (`{ marker: u8 = 5 }`) | Typeck error `expected u8 found i32` | Bidirectional check_expr should narrow ComptimeInt-typed literals to expected width if value fits |
| Top-level statements (no `main()`) | Parser rejects | **Increment 11** — see below |
| Implicit `Print` of bare string literals | No string slice type, no Print | **Increment 11** — see below |
| String literals beyond passing as opaque pointers | Typeck records `Ty::Error` for `StringLit` | Needs `[]u8` slice type (Phase 2 or "Phase 1.5") |
| Multi-segment paths in expressions (`std::mem::Foo`) | Typeck `UNSUPPORTED_CONSTRUCT` | Phase 2 (frequencies / module imports) |
| `match`, error unions (`!T`), generics, `cipher`, async, comptime | Parser produces `ErrorNode`s | Phases 2–4 |
| Float arithmetic in tests | MIR/codegen support exists but no corpus exercises it | Add corpus programs (low-hanging) |
| LLVM backend | `arsenal_codegen_llvm` stub only | **Increment 13** — not session-blocking |
| `arsenal new` template parses cleanly | Templates use `#virtuous {}` and bare-string-literal syntax that Phase 1 parser rejects | Either swap templates to Phase-1 syntax or block on increment 11 |

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

---

## Next increment — 11

**Goal:** top-level statements + implicit `Print` of bare string literals.
This is the last meaningful Phase-1 surface before increment 12 (corpus
expansion to 200 programs) and increment 13 (LLVM port).

### Sub-task split (recommended)

The increment naturally splits into three sub-pieces. Land them as separate
commits:

#### 11a — top-level statements (no I/O)

A `.gw` file currently must contain only items (`fn`, `class`, etc.). After
11a, top-level statements are also valid:

```gw
let x: i32 = 5;
let y: i32 = 7;
return x + y;     // exits 12
```

This requires:

- **Parser**: `parse_module` accepts items OR statements at the top level.
  Need a way to distinguish: items have leading `pub`/`extern`/`fn`/`class`
  (already detected by `peek_item_keyword`); anything else falls through to
  `parse_stmt`. New SyntaxKind variant — probably `TopLevelStmt` — or just
  reuse the existing Stmt variants as direct Module children.
- **Resolver**: top-level `let` bindings get DefIds (or stay anonymous?
  decide). For Phase 1, just collect them as a synthetic `_start` body.
- **Typeck**: type-check the synthetic body, return type `i32` (POSIX exit).
  The trailing `return` is the program's exit code; any value-yielding stmt
  at the end is the implicit return.
- **MIR**: lower the synthetic body as a function named `main`. If the user
  also declared an explicit `fn main` somewhere, that's a name collision —
  diagnose.
- **Driver**: `arsenal build` accepts files that are pure top-level
  statements (no `fn main`).

The corpus program for 11a is roughly:
```gw
let x: i32 = 5;
let y: i32 = 7;
return x * y;     // exits 35
```
plus a few variants exercising loops + classes at the top level.

#### 11b — string slice type (`[]u8`)

Phase 1 currently records `Ty::Error` for string literals — there's no slice
type. Adding `[]u8` is meaningful even before Print:

- **`Ty::Slice(elem_ty)`** in typeck — fat pointer (data ptr + length).
  Phase 1 immutable only.
- **MIR**: a slice operand is a `(ptr, len)` pair. Could lower as two
  primitive locals or as a 2-field "slice descriptor" stack slot.
- **Codegen**: pointer-sized + pointer-sized, two stack-slot fields for
  small slices; or a synthetic class layout with `data: ptr` and `len: usize`.
- **String literals in code**: emit the bytes into a `.rodata` section
  (Cranelift's `module.declare_data` + `define_data_object`), and at the
  literal use site emit a slice descriptor pointing at it.

This is itself a significant chunk (~300-500 LoC). Could be skipped in the
first session pickup if scope tightens — only blocks 11c.

#### 11c — implicit Print of bare string literal

Per spec §5.15.1: `"hello\n";` at statement position is sugar for
`Print("hello\n")`. With slices in hand:

- **Parser/typeck**: at *statement* position (only), an `ExprStmt` whose
  expression is a `LiteralExpr` of `StringLit` desugars to a call to a
  builtin Print fn.
- **`philosophers.fmt.print`**: the natural place to define Print, but we
  don't have a stdlib yet. Phase-1 shortcut: declare a builtin
  `extern fn _gw_print_str(ptr: *u8, len: usize) -> u0;` that lowers to a
  `write(2, ptr, len)` syscall (or `fwrite` to stdout). Simplest: add
  `extern fn write(fd: i32, buf: ptr, count: usize) -> isize;` to the
  prelude and call it directly.

The first Phase-1 hello world becomes:
```gw
"Behold the Outer Heaven.\n";
```
↓
```gw
fn main() -> i32 {
    write(1, "Behold the Outer Heaven.\n".data, 25);
    return 0;
}
```
↓ Cranelift → executable that prints and exits 0.

### Open design questions for 11

These need a decision *before* writing code. Surface them at session start:

1. **Synthesised entry point**: name `main` (collision risk) or `_start`
   (still collides with libc on macOS). Architecture Part L Phase 1 says
   "synthesize a `_start` from top-level statements". Spec §5.15.1 implies
   `main()` is wholly absent. Pick one.
2. **Implicit return type**: top-level statements should exit with code 0
   if no explicit return; that means MIR's synthetic main returns i32 with
   a trailing `Return(Const::Int 0)` if no `return` stmt fired. The
   architecture Part L Phase 1 says "Hello World" should be the canonical
   demo — so the trailing exit = 0 must Just Work.
3. **Mixing top-level stmts with explicit `fn main`**: error or shadow?
   Recommendation: error, since the synthetic main and explicit one would
   collide on the symbol.
4. **String literal storage**: rodata section vs heap-allocate at startup?
   Architecture says rodata. Cranelift's data API supports it but I
   haven't used it yet — there's a small learning curve.

### Files that will need editing for 11

- `arsenal_parse/src/grammar.rs` — extend `parse_module` to accept stmts.
- `arsenal_ast/src/ast.rs` — Module accessor for top-level stmts.
- `arsenal_resolve/src/lib.rs` — synthesize an "implicit main" def from
  top-level stmts.
- `arsenal_typeck/src/lib.rs` — type-check the synthesized body.
- `arsenal_mir/src/lib.rs` — lower the synthesized body.
- `arsenal_codegen_fast/src/lib.rs` — for 11b/c: declare data objects
  for string literals; module.declare_data + define_data_object.
- `arsenal_driver/src/cmd_build.rs` — small UX nit: handle the case
  where compilation produces a `main` even though no `fn main` was
  written.
- New corpus files in `tests/snake_eater/pass/phase1/72_*.gw` onward.

### Cost estimate

- 11a alone: ~2-3 hours, ~250 LoC.
- 11a + 11b: ~4-6 hours, ~500-700 LoC. String slices need storage.
- 11a + 11b + 11c: probably the rest of a session.

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
git log --oneline | head -3
# expect: c7870bb at top, then 84aa0eb, then cb796fe

git status
# expect: clean working tree (no .DS_Store, no .probe leftovers)

. "$HOME/.cargo/env"
cargo test --manifest-path compiler/arsenal-boot/Cargo.toml --workspace 2>&1 | grep "test result" | awk '{p+=$4;f+=$6}END{print p,f}'
# expect: "95 0"

ls tests/snake_eater/pass/phase1/*.gw | wc -l
# expect: 71

ls compiler/arsenal-boot/crates/ | wc -l
# expect: 17
```

If any of those fail, **don't start increment 11** — investigate first.
The most likely culprits are stale `target/` directories, an outdated
`Cargo.lock`, or someone else's commits between sessions.

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
one of those arrows. The next increment (11) extends the leftmost arrow
(parser accepts top-level stmts) and adds two new operations near the
rightmost (string-literal data section + implicit Print desugar). The
arrows themselves don't change shape.
