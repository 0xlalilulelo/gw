# GW Programming Language — Comprehensive Technical Development Document

*A Metal-Gear-Solid–themed systems language fusing HolyC's interactive single-pass philosophy with the modern systems-language design space defined by Zig, Odin, Jai, and Rust.*

---

## Part A: Host Language Recommendation

### A.1 Evaluation Criteria

The host language for the GW bootstrap compiler must support the following workloads efficiently:

1. **Heavy AST/IR traversal** — pattern matching over deeply nested sum types is the dominant operation in every pass.
2. **Arena-style allocation** — ASTs, types, MIR, and LIR are produced and discarded in bulk; manual or arena allocation outperforms generic GC.
3. **LLVM integration** — the bootstrap backend depends on LLVM C/C++ APIs.
4. **Single-binary distribution** — `arsenal` is shipped as one statically linked executable across Linux/macOS/Windows.
5. **Debuggability** — when the compiler crashes inside the borrow checker or comptime engine, stack traces, conditional breakpoints, and value inspection must be first-class.
6. **Bootstrap erasure** — the host language is a temporary scaffold; the rewrite into self-hosted GW must be mechanical, not philosophical.
7. **Parallel/incremental compilation** — the compiler eventually shards work per-frequency (module) and per-function; data-race freedom in the host is highly desirable.

### A.2 Comparison

| Criterion | OCaml | Rust | C++ |
|---|---|---|---|
| Sum types (ADTs) | Native, exhaustive, ergonomic | Native (`enum`), exhaustive, slightly verbose | `std::variant` + `std::visit`; clumsy |
| Pattern matching | Best in class — nested, guards, `as`, or-patterns | Excellent; ref-patterns trip people up | None natively; `if constexpr` ladders |
| GC vs manual | Generational GC, very fast minor cycles | No GC; arenas via `bumpalo`/`typed-arena` | Manual; arenas trivial; UAF risk |
| Performance (compiler workloads) | Within ~1.5–2× of Rust on AST-heavy code; faster compile-edit-run loop | Top-tier; data-oriented designs (rustc, Zig-in-Zig style) achievable | Top-tier, but build times worst of three |
| LLVM bindings | `llvm` opam package; lags releases by 6–12 months | `inkwell`, `llvm-sys`; well-maintained, multiple LLVM versions | Native — first-class consumer of LLVM C++ API |
| Parser ecosystem | Menhir, Sedlex, ocamllex — the gold standard | `logos`, `chumsky`, `lalrpop`, hand-written recursive descent dominant | hand-written; ANTLR/Bison painful |
| Terminal UI / LSP / file watch | `notty`, `lwt`, `lsp` library exists; smaller ecosystem | `crossterm`, `tower-lsp`, `notify`, `tokio` — saturated, production-quality | Hand-rolled or pull in Boost; heavy |
| Single-binary distribution | Native via `dune`, statically linkable; some C deps | Trivial via `cargo`; musl static builds standard | Requires CMake + careful static linking; libc++/libstdc++ ABI woes |
| Parallel compilation | OCaml 5 has effect handlers + domains, but ecosystem still maturing for shared-memory | `rayon`, `crossbeam`, `dashmap`; battle-tested, fearless concurrency | Threads + manual sync; data races common |
| Incremental/query-based design | Naturally functional; memoization libraries fit well | rustc itself proves this works; salsa/rust-analyzer pattern | Possible but verbose; lifetime tracking by hand |
| Debugger UX | `ocamldebug` time-travel; printf often dominates | `lldb`/`gdb` with pretty-printers; mature | `gdb`/`lldb` excellent; symbol bloat in templates |
| Compile-edit-run loop on the host | ~1–3 s for medium project — *the* OCaml superpower | 10–60 s incremental; cold builds painful | 30 s–several minutes |
| Bootstrap erasure (mechanical port to GW) | Clean: ADT→`liberty`, modules→frequencies, GC→arenas | Cleaner: ownership semantics already align with GW; types map almost 1:1 | Doable but every `unique_ptr`, `shared_ptr`, template trick must be re-thought |
| Memory safety in compiler internals | GC + immutability defaults | Borrow check; some impedance vs graph-shaped IRs | None |
| Hash maps | `Hashtbl`, `Map.Make(...)`; functorial | `HashMap`, `FxHashMap`, `IndexMap`, `DashMap`; rich ecosystem | `std::unordered_map` (slow), `absl::flat_hash_map` (must vendor) |

### A.3 Recommendation: **Rust**

Rust is recommended as the host language for the GW bootstrap compiler. The decision is driven by four dispositive factors and one strategic factor.

**1. Semantic alignment with GW.** GW's type system — ownership-flavored borrow checking, sum types via `liberty`, traits via `cipher`, monomorphized generics, error unions, and `Send`/`Sync` auto-derivation — is structurally Rust with a Metal Gear paint job. Every concept the bootstrap compiler manipulates (lifetimes, regions, trait selection, monomorphization) has a near-isomorphic representation in Rust. When the time comes to port to self-hosted GW (Phase 6), the translation is *largely mechanical*: `enum` → `liberty`, `trait` → `cipher`, `Vec<T>` → `[dyn]T`, `Result<T, E>` → `!E T`, `?` → `foxdie`. OCaml's GC and Hindley–Milner globalism would force genuine reformulation; C++'s `unique_ptr`-heavy idioms would also.

**2. Ecosystem completeness for compiler tooling.** Every tool in the `arsenal` driver has a best-in-class Rust crate: `tower-lsp` for the language server, `notify` for file watching, `crossterm`/`ratatui` for the REPL UI, `rayon` for parallel compilation, `dashmap` for concurrent symbol tables, `inkwell`/`llvm-sys` for LLVM, `gimli`/`object` for DWARF and ELF/Mach-O/COFF, `cranelift` available as a fallback fast backend during early phases, `salsa` if a query-based incremental architecture is desired. OCaml's compiler-tooling ecosystem (Menhir, Sedlex) is excellent for the *front end* but thins out at the LSP/LLVM/cross-platform-IO frontier. C++ requires re-implementing or vendoring most of these.

**3. Fearless parallelism.** GW targets parallel per-frequency compilation and a multi-threaded LSP. Rust's `Send`/`Sync` discipline means that when the compiler scales out, data races become impossible by construction. OCaml 5's domains work, but the shared-memory parallel ecosystem is years behind. C++ leaves race-freedom as an exercise for the reader.

**4. LLVM integration without C++ buy-in.** `inkwell` provides a safe, idiomatic LLVM wrapper with versioned features (`llvm17-0`, `llvm18-0`, …). The team avoids C++ build hell while retaining full LLVM access. For the fast backend (TPDE-style), Rust's lower-level facilities (`std::arch`, `byteorder`, `region`, `memmap2`) are sufficient to emit machine code directly to RWX pages.

**5. Strategic: contributors.** A compiler advertised as Rust + LLVM attracts the same contributor pool that builds rustc, rust-analyzer, Cranelift, and the major Rust language servers — a pool that already understands borrow checking, monomorphization, and trait resolution.

### A.4 What OCaml is best at, and why we still pass

OCaml *would* yield a smaller, more elegant front end (lexer + parser + type checker) faster — perhaps 30–40% less code, with a tighter inner-development loop. For a *research* compiler, OCaml is the right answer; the F\*, Coq, Flow, ReScript, Hack, and rustc-stage0 lineage proves it. GW, however, is a production systems language whose host compiler must ship a JIT, a fast backend, an LSP, a package manager, and a cross-compiler, all in one binary, on day one of public release. Rust pays its overhead at the front end but recoups it everywhere else.

### A.5 What C++ is best at, and why we still pass

C++ is the only language with a *first-class* relationship to LLVM (LLVM is itself C++). A C++ host gives access to MLIR, SelectionDAG internals, and TPDE (which is C++) without bindings overhead. But: build-system pain, lifetime bugs, lack of sum types, and a hostile distribution story (libstdc++ vs libc++, MSVC ABI) are non-trivial taxes. The TPDE technique is portable; we can re-implement its template-driven encoder in Rust referencing the published paper and the C++ reference implementation.

### A.6 Bootstrap Strategy

The bootstrap pathway has six stations:

1. **Stage 0 — Rust scaffold (Phases 0–4).** The host-language compiler `arsenal-boot` is written in Rust. It targets LLVM only. It accepts the GW subset needed to compile itself.
2. **Stage 1 — Self-hosting subset frozen (end of Phase 5).** A documented "GW₀" subset is locked: it omits async, omits the fast backend, omits comptime metaprogramming beyond `#run` of pure functions, omits inline `rex` blocks in the compiler. The Rust compiler is feature-frozen against GW₀.
3. **Stage 2 — Translation (Phase 6).** The Rust compiler is ported to GW₀ module-for-module. The translation is reviewed for semantic equivalence rather than literal correspondence. The Rust compiler remains canonical.
4. **Stage 3 — Crossover.** `arsenal-boot` (Rust) compiles `arsenal-self` (GW source) → `arsenal-1` (binary). `arsenal-1` then compiles itself → `arsenal-2`. **Byte-for-byte equality of `arsenal-2` and `arsenal-3 = arsenal-2(arsenal-self)` is the bootstrap acceptance test** (the standard "fixed-point" check; same as Go, Rust, Zig).
5. **Stage 4 — Rust retirement.** The Rust source is moved to `bootstrap/legacy/` and frozen. Future bootstraps use the previous-release `arsenal` binary (Zig-style two-version chain) plus a checked-in `arsenal-stage1.wasm` (per Zig 0.11+) to avoid an indefinite Rust dependency in CI.
6. **Stage 5 — Bootstrap from source.** A `bootstrap.c` shim — a tiny, hand-written C program that interprets a frozen GW IR snapshot — replaces the WASM blob for distribution-friendly source bootstrap (cf. mrustc, Guix's full-source bootstrap effort).

The crossover happens at the Phase 6 boundary, *before* the fast backend (Phase 7). This is deliberate: writing a TPDE-style backend is more pleasant in GW than in Rust, because GW's `rex { }` blocks express encoder templates more directly than Rust's `asm!` macros. Self-hosting *enables* the fast backend rather than blocking on it.

---

## Part B: Compiler Architecture

### B.1 Pipeline Overview

```
.gw source  ──►  Lexer  ──►  Parser  ──►  AST
                                            │
                              ┌─────────────┘
                              ▼
                          Resolver  (name binding, frequency graph)
                              │
                              ▼
                          Type Checker  (bidirectional, cipher resolution)
                              │
                              ▼
                          Comptime Engine  (interleaves with type check)
                              │
                              ▼
                          MIR Builder  (CFG + SSA, lifetime annotations)
                              │
                              ├──► Borrow / Lifetime Checker
                              │
                              ▼
                          MIR Optimizer  (inline-small, const-prop, dead-code)
                              │
                              ▼
                          LIR Lowering  (target-shaped, register-flavored)
                              │
                       ┌──────┴──────┐
                       ▼             ▼
                  Fast Backend     LLVM Backend
                  (TPDE-style)     (release / cross)
                       │             │
                       ▼             ▼
                  in-memory JIT   object files → lld → executable
```

### B.2 Lexer

UTF-8-native, written as a state machine on `&[u8]` with a small lookahead. Token stream is produced lazily; for incremental builds it is checkpointable per line. Reserved words are interned at startup into a `phf` perfect-hash map. Numeric literals carry a "literal kind" tag (`int`, `float`, `rune`, `string`, `byte_string`, `markdown_fence`) to support GW's bare-string-prints implicit-Print rule. The lexer also recognizes `/// doc comments` and emits them as token-attached trivia rather than discarding, so the doc generator can later reconstruct comment ownership.

### B.3 Parser

A hand-written **recursive-descent parser with Pratt-style expression precedence**. Hand-written is non-negotiable: error recovery, location precision, and IDE responsiveness all benefit. The parser is **error-recovering** (panic-mode at statement boundaries; bracket-counted skip), so the LSP can produce a partially valid AST after every keystroke. The grammar is described in `docs/grammar.ebnf` for reference, but no parser-generator is in the dependency graph — Menhir-style tools complicate error recovery and introduce a build-time dependency.

The parser attaches **CST nodes** (concrete syntax trees, à la rust-analyzer's rowan) in addition to ASTs. The CST preserves trivia (whitespace, comments) for `arsenal fmt` and rename refactors. ASTs are derived as a typed view over the CST.

### B.4 Single-Pass with Backpatching

GW inherits HolyC's "no headers, no forward declarations" principle. The compiler is "single-pass" in the user-visible sense (one source traversal forces compilation), but internally:

- **Top-level items are collected before bodies are checked.** The first traversal records signatures, class layouts, cipher declarations, and constants; bodies and use-site expressions are checked in a second sub-pass within the same compiler invocation. This is how Pascal, D, and Crystal achieve "no headers" without truly going one-token-at-a-time.
- **Forward references are tracked through a fixup table.** When name resolution encounters an unresolved identifier in a body, the AST node is inserted into a `pending: HashMap<Symbol, Vec<NodeId>>`. When the symbol is later defined, all pending nodes are revisited. A topological sort detects cycles (illegal except inside function bodies and lazy `class`/`liberty` self-references through pointers).
- **Mutually recursive functions** are supported because signatures are fully resolved before *any* body is type-checked.
- **Mutually recursive type definitions** require a pointer/optional/slice indirection (like Rust, Swift); the compiler detects illegal infinite-size cycles via SCC analysis on the type graph.

### B.5 Resolver

The resolver builds a **frequency graph** (module DAG): each `.gw` file contributes declarations to its directory's frequency. `MotherBase.gw` declares the manifest. Names within a frequency are visible without qualification; cross-frequency references go through `import "philosophers/io"`. The resolver populates a per-frequency `SymbolTable` keyed by `(scope, name)` and produces an `ItemId → DefId` mapping.

UFCS (uniform function-call syntax) means `x.foo(y)` first resolves to `Foo::foo(x, y)` if `x: Foo`, otherwise falls through to free functions. The resolver records both candidates and defers the decision to type checking when the receiver type is concrete.

### B.6 Type Checker

See Part D for the algorithm. Architecturally, the type checker:

1. Visits each item in topological order over the signature graph.
2. For each function body, runs **bidirectional inference** producing a typed AST.
3. Records cipher obligations into a constraint set, solved before MIR lowering.
4. Drives the **comptime engine** for `#run`, `#virtuous { }`, and generic instantiations whose argument is a `comptime` value.
5. Invokes generic monomorphization on demand; instantiated functions are cached by `(generic_def_id, type_args_hash)`.

### B.7 MIR Builder & Borrow Checker

The MIR is a per-function CFG of basic blocks containing three-address SSA instructions plus *region annotations* — every reference value carries a `Region` ID that lives until end-of-scope (function-local, simpler than Polonius). Borrow checking is a forward dataflow over MIR; see Part D.

### B.8 LIR Lowering

LIR is target-shaped: it speaks of physical-style virtual registers, knows about calling conventions, lays out stack frames, and resolves `class` field accesses to concrete byte offsets. From LIR, both backends emit their final output.

### B.9 Incremental Compilation

A **per-frequency cache** keyed on the SHA-256 of (source bytes ∪ public signatures of imported frequencies) stores:

- Parsed AST (RKYV-serialized for zero-copy load)
- Resolved symbol table fragment
- Type-checked function bodies (MIR)
- Monomorphized instantiation index
- Object files (when AOT)

When a frequency's input hash matches the cache, downstream passes reuse the cached MIR. Function-level granularity (rustc-style red/green query DAG) is **deferred to Phase 10**; per-frequency granularity is sufficient for early phases and is what Zig and Odin currently ship.

### B.10 Parallel Compilation

Per-frequency parallelism via `rayon::scope`. Within a frequency, function-body type checking is independent once signatures are resolved, allowing intra-frequency `par_iter` over function bodies. The fast backend is embarrassingly parallel at function granularity. LLVM module emission is parallel per "code-gen unit" (one per frequency by default).

### B.11 Comptime Engine

See Part E. Architecturally, the comptime engine is a **stack VM** operating on MIR. It is invoked from inside the type checker; results are reified back into the typed AST as constants. Sandboxing prevents I/O, network, syscalls, and unbounded computation.

---

## Part C: IR Design

### C.1 Why Three IRs

A single IR conflates concerns; two IRs (AST + LLVM-IR) loses information needed for borrow checking and forces a re-lowering for the fast backend. GW uses **three** levels:

| IR | Form | Purpose | Consumers |
|---|---|---|---|
| **AST** (typed) | Tree, source-shaped | Type checking, comptime, reflection (`@codec`) | Type checker, comptime engine, LSP, formatter, doc generator |
| **MIR** | CFG of SSA basic blocks, region-annotated | Borrow check, comptime VM, MIR-level optimizations | Borrow checker, comptime VM, MIR opt passes |
| **LIR** | Linear, register-flavored, target-aware | Codegen — single representation feeding both backends | Fast backend, LLVM backend |

Three IRs match rustc's HIR/MIR/LLVM-IR split and Zig's ZIR/AIR/MIR split. The benefit is that each pass operates on the right shape: ASTs preserve user intent for reflection, MIR is the right shape for dataflow, LIR is the right shape for register allocation.

### C.2 AST

The AST is a typed, post-resolution tree. Each node carries:
- A `NodeId` (stable across incremental rebuilds via positional hashing within file)
- A `Span` (byte range in source, plus file ID)
- A `Type` (resolved post type-check) or `TypeVar` (during inference)
- Attached `attributes` (e.g., `@range(0,100)`, `@serialize`) — these survive into MIR/LIR for codegen-time use and into reflection metadata

Class field metadata is preserved through compilation by storing it in the type table indexed by `(ClassDefId, FieldIdx)`. Reflection at comptime reads from this table directly.

### C.3 MIR

**SSA form**, basic blocks with explicit terminators (`Goto`, `Branch`, `Switch`, `Return`, `Unreachable`, `Call`, `Resume`, `Drop`, `Yield`-for-async). Operations:

```
rvalue := Use(operand) | BinOp(op, l, r) | UnOp(op, x)
        | Ref(region, mut, place) | Deref(place) | Cast(kind, x, ty)
        | Aggregate(kind, fields) | NullaryOp(SizeOf|AlignOf|TypeId, ty)
        | CheckedBinOp(op, l, r)   // overflow-checked in safe mode
        | Discriminant(place)      // for liberty tag reads

statement := Assign(place, rvalue)
           | StorageLive(local) | StorageDead(local)
           | SetDiscriminant(place, variant)
           | RegionStart(r) | RegionEnd(r)        // lifetime markers
           | Retag(place)                          // safety tier transition

terminator := Return | Goto(bb)
            | SwitchInt(operand, [(value, bb)], default_bb)
            | Call { func, args, dest, target_bb, unwind_bb }
            | Drop { place, target_bb }
            | Assert { cond, msg, target_bb, unwind_bb }
            | FoxdiePropagate { error, target_bb }
            | Unreachable
```

SSA is chosen (vs non-SSA) because:
1. The fast backend (TPDE) **requires** SSA input.
2. The LLVM backend benefits — we can emit LLVM-IR via a near-identity transform.
3. Borrow checking on SSA is cleaner: each definition site is unique; phi nodes make convergence explicit.

Classical Cytron et al. SSA construction with dominance frontiers; "minimal" SSA is sufficient — pruning is not needed for borrow check.

**Region annotations** are a separate side table: `RegionMap : Local → Region`. Regions are introduced at `let` bindings and `&` expressions, ended at scope exit. The borrow checker consults this side table.

### C.4 LIR

**Target-tagged but architecture-agnostic until selection**. LIR is linear (instructions listed in a flat vector per function with explicit control-flow markers), register-flavored (operands are `VirtualReg` IDs), and ABI-aware (calls already laid out for the platform's calling convention).

Instruction set is a sea of simple opcodes mirroring SelectionDAG + a few high-level ones (gep, load, store, atomic, branch, call, ret, alloca). Register allocation runs over LIR for the fast backend; the LLVM backend simply pretty-prints LIR to LLVM-IR text or via `inkwell`.

### C.5 IR Serialization

Both MIR and LIR have stable on-disk forms used by the incremental cache:
- **Encoding**: `rkyv` for zero-copy load (Rust host); switch to a custom GW-native binary encoding post-bootstrap.
- **Versioning**: each cache entry tagged with `(compiler_version_hash, frequency_input_hash)`.
- **Granularity**: per-function MIR; per-frequency LIR (LIR is cheap to regenerate from MIR but cached for parallel codegen).

---

## Part D: Type Checker & Lifetime/Borrow Checker

### D.1 Type Inference Algorithm

GW uses **bidirectional type inference** in the style of Dunfield & Krishnaswami's "Complete and Easy Bidirectional Typechecking for Higher-Rank Polymorphism" (2013). Two judgments interleave:

```
Γ ⊢ e ⇒ τ      "expression e synthesizes type τ"     (synthesis / infer)
Γ ⊢ e ⇐ τ      "expression e checks against τ"        (checking)
```

Inference rules:
- Variables, literals (when context-free), function applications synthesize.
- Lambda bodies, branches of `if`/`switch`, `liberty` constructors check against context.
- Unification variables (`?T`) are introduced for unannotated locals and resolved via constraint propagation.
- Higher-rank polymorphism is *not* exposed to users; internally, generic functions are polymorphic over `cipher`-bounded type variables.

This algorithm is preferred over Hindley-Milner because:
1. Better error messages (errors are localized at the synthesis/checking boundary).
2. Decidable in the presence of cipher trait bounds and overloading via UFCS.
3. Supports literal coercion (e.g., `5 ⇐ u8` succeeds; `5 ⇒ comptime_int` synthesizes).
4. Trivially extends to GW's `comptime_int`/`comptime_float` story (cf. Zig).

### D.2 Generic Instantiation

Monomorphization: each `(generic_fn, concrete_type_args)` pair produces a fresh specialized function, cached in a global `InstantiationTable: HashMap<(DefId, TypeArgsHash), MonoDefId>`. Cipher bounds are solved at instantiation time.

To control code bloat, **shared monomorphization** is implemented for cipher-only-using generics: when a generic uses its type parameter only through cipher-method calls (no struct field access, no size queries), we generate one polymorphic body parameterized by a vtable, rather than monomorphizing per type. This is opt-in via `#[shared]` initially; auto-detection is a Phase 10 optimization. This mirrors rustc's planned but unshipped polymorphization pass and Swift's reabstraction approach.

### D.3 Cipher (Trait) Resolution

Cipher resolution is a constraint-solving pass:

1. From use sites, collect goals: `T : Arsenal`, `[T]: Iterator<Item=T>`, etc.
2. Search the `impl` table (every `cipher T for U { }` registers an entry).
3. Coherence check: at most one impl per `(cipher, type)` pair within a compilation. Cross-frequency conflicts are resolved by orphan rules — an impl is legal iff either the cipher or the type is defined in the current frequency.
4. Specialization is **not** supported in GW₀ (avoids unsoundness traps that have plagued Rust for a decade).
5. Auto-derived ciphers (`Send`, `Sync`, `Copy`) are inferred structurally: a class is `Send` iff all fields are `Send`.

### D.4 Comptime Type Construction

Types are first-class at comptime. `@codec(T)` returns a `TypeInfo` value; functions can take `type` parameters; types can be returned from functions and assigned to constants. The type checker invokes the comptime engine for any expression whose evaluation yields a type, and reifies the result back into the type table.

### D.5 Lifetime / Borrow Checker

GW chooses **function-local, region-based** borrow checking — essentially Rust's NLL but stopping short of Polonius's location-sensitive subset constraints. The rationale: a simpler checker is easier to specify, faster to run, and sufficient for the safety property we want; users who hit edge cases can drop to `manual` tier.

Algorithm (per function, on MIR):

1. **Region inference.** Each `&` introduces a fresh region variable. Regions form a partial order from `outlives` constraints arising from assignments, function arguments, and return types.
2. **Loan tracking.** At every program point, maintain a set of *active loans* — each loan is `(place, mut?, region)`. A loan starts at the `&` and is invalidated when its region is no longer in scope (forward dataflow with the lattice ⟨2^Loans, ⊆⟩).
3. **Aliasing rule check.** At each access of place `p`:
   - If access is mutable: assert no loan covers `p` or any prefix/extension of `p`.
   - If access is shared: assert no *mutable* loan covers `p` or related places.
   - "Covers" uses path-prefix analysis on places (`x.f` covers `x.f.g`; `x` covers `x.f`).
4. **Move tracking.** Moves invalidate the source. The dataflow lattice tracks `MaybeInitialized` and `EverInitialized` per local. Drop elaboration inserts `Drop` terminators where definitely-initialized goes to definitely-uninitialized.
5. **`defer` and `errdefer`.** Lowered to inserts of inverse statements at every scope-exit edge during MIR construction. The borrow checker treats deferred bodies as if they execute at end-of-scope; any borrow they hold extends accordingly.
6. **Tier transitions.** `manual` blocks suppress the aliasing check but retain initialization tracking. `naked` blocks skip both.

This is "Polonius lite": **scope-bounded regions, no loans-in-scope-per-location precision**. We accept that some programs Rust accepts (problem case #3) GW will reject; users use `manual` or restructure. The win is a checker measured in tens of microseconds per function rather than milliseconds.

### D.6 Error Reporting

Every diagnostic carries:
- A primary span (the locus of the error).
- Zero or more secondary spans with labels (e.g., "earlier borrow occurs here").
- A `note:` chain explaining why the borrow conflicts (chained from region inference).
- A suggested fix (e.g., "consider using `.clone()`" — emitted only when ≥ 90% confident).
- A stable error code (`E0042`-style) for documentation lookup.

For lifetime errors specifically, the checker prints the **smallest counterexample MIR fragment** annotated with the offending loan and the conflicting access, in the style of the NLL-era rustc output.

---

## Part E: Comptime Engine

### E.1 Design Choice: Stack VM

Initial implementation (Phase 2) is a **tree-walking interpreter** on the typed AST — fast to build, easy to debug, sufficient for `#run` of small expressions. By Phase 5 it is replaced with a **stack VM operating on MIR** — Zig's approach. The stack VM:

- Reuses the regular MIR pipeline (no separate IR for comptime).
- Yields ~10–50× speedup over tree-walking on heavy generic instantiation.
- Shares code with the borrow checker's MIR walker for sanity.
- Naturally supports save/resume for incremental builds (a comptime computation that depends only on unchanged inputs is cached).

The VM is a **values-on-stack, locals-in-frame** design: each frame has a fixed-size locals array (sized from MIR), an operand stack, and a pointer back to the caller's frame. Memory for comptime allocations comes from a per-invocation arena, freed when the top-level comptime invocation returns.

### E.2 Sharing Semantics with Runtime Types

Comptime values inhabit the same type system as runtime values, but with extra types: `type` (a type-of-types), `ComptimeInt`, `ComptimeFloat`, and `Tuple` literals before destructuring. Coercion rules say a `comptime_int` literal coerces to any concrete integer type that fits; failure to fit is a compile error.

Pointers at comptime cannot escape to runtime (no comptime memory survives compilation), but values can be "lowered" — a `[3]i32` known at comptime becomes a constant in `.rodata`.

### E.3 Sandboxing

- **No I/O by default.** No `philosophers.fs`, `philosophers.net`, `philosophers.os` calls reachable. The standard library marks these modules `#[no_comptime]`; the comptime VM refuses to dispatch.
- **Memory cap.** A configurable budget (default 256 MiB per top-level comptime invocation). Allocation beyond the cap triggers a comptime error.
- **Operation cap.** Default 10⁹ VM steps. Configurable per-project in `MotherBase.gw`. Exceeding the cap is a hard error: `error: comptime evaluation exceeded operation budget (suspected infinite loop)`.
- **Recursion depth cap.** Default 1024 frames.
- **Determinism.** No access to `Now()`, `RandomBytes()`, environment variables, file system. Hash maps used at comptime use a fixed seed; iteration order is deterministic.
- **`#[allow_comptime_io]`** attribute on a build script can opt in to file reads (for `#asset`, codegen from schemas), but never to network access. Reads are recorded; the cache invalidates if any read file's content hash changes.

### E.4 `#run`, `#insert`, `#virtuous`

- `#run expr` — evaluates `expr` at comptime, replaces the call site with the resulting constant (Jai-style).
- `#insert(s)` — `s` must be a `comptime []u8`; the bytes are re-fed to the lexer and parser at the call site (Jai-style code injection). Inserted source is sandboxed: it runs with the caller's lexical scope but cannot define top-level items. Used for procedural macros and AST builders.
- `#virtuous { ... }` — Zig-style block executed entirely at comptime; declarations within escape to the surrounding scope. Used for compile-time configuration, conditional compilation, and embedded-code generation.

### E.5 Reflection Intrinsics

`@codec(T)` returns a `TypeInfo` `liberty` whose variants mirror `std.builtin.Type` from Zig. Field metadata (`@range`, `@serialize`, custom user attributes) is exposed via `info.fields[i].attrs`.

`@field(v, "name")` is comptime-string-resolved via the type table, lowering to a direct field access at runtime.

`@call(f, args_tuple)` produces a call site whose argument list is splatted from the comptime tuple — used for variadic generic forwarding.

`inline for f in info.fields { ... }` unrolls in MIR construction: the comptime engine produces N copies of the loop body with `f` substituted, all merged into the surrounding CFG.

---

## Part F: Code Generation

### F.1 Fast Backend (TPDE-style)

The fast backend follows the TPDE 2025 paper (Schwarz, Kamm, Engelke, TUM): a single-pass code generator that combines instruction selection, register allocation, and encoding. We adapt the technique to GW's LIR.

**Architecture.**

1. **Single linear pass over LIR per function.** No separate RA pass; allocation happens as instructions are encoded.
2. **Template-driven encoding.** Each LIR opcode has, per target (x86_64, aarch64), a small set of *encoding templates* — short byte sequences with placeholders for register IDs and immediates. Templates are derived from a high-level DSL (mirroring TPDE-Encodegen, which uses LLVM's MachineIR; we hand-write or extract from Cranelift's emit tables initially, then move to a DSL post-self-hosting).
3. **Linear-scan register allocation, simplified.** A "live-range bitmap" per register tracks which virtual registers occupy each physical register at each point. On allocation pressure, the oldest-defined value is spilled to the stack frame.
4. **Backpatching.** Forward branches are recorded and patched when the target block is encoded.
5. **Direct emission.** Output is either an in-memory byte buffer (JIT) or a per-function buffer assembled into ELF/Mach-O/COFF object sections.

**Achieving 1M LoC/s/core.** TPDE achieves 8–24× faster compile times than LLVM `-O0` and ~4× faster than Cranelift. The bottlenecks at the 1M LoC/s target are:

- **Lexing/parsing** dominates if we are not careful. Mitigations: hand-written lexer with branchless UTF-8 decoding for ASCII fast-path; one-allocation-per-file AST arenas.
- **Hash map lookups** for symbol resolution. Mitigations: `FxHashMap` (no DoS resistance needed inside compiler); per-frequency string interner with tiny hash via FxHash; hot-path symbol caches.
- **Memory allocator overhead.** Mitigations: bump-allocated arenas per pass; no per-node `Box`.
- **Register allocator.** Linear scan is linear-time; the constant factor is what matters. Use `u64` bitmaps for register sets; avoid sorting.
- **Encoding.** Direct memcpy of templates plus immediate patching. No assembly text intermediate.
- **Object file emission.** Pre-compute section layouts; write headers last.

The 1M LoC/s figure refers to **non-comptime**, non-async GW source on a single core, post-cache-warm, on a Zen 4 / Apple M-series machine. Code with heavy generics will be slower; the figure is an *aspirational throughput for typical application code*, in the spirit of D's `dmd`, Jai's reported numbers, and Pascal compilers of the 1990s.

### F.2 LLVM Backend

LIR-to-LLVM-IR via `inkwell`. Each LIR opcode maps to a small group of LLVM IR instructions (most map 1:1). GW's safety semantics translate to LLVM's `nounwind`, `noalias`, `dereferenceable`, `align` attributes. The LLVM backend:

- Supports `-O0`, `-O1`, `-O2`, `-O3`, `-Os`, `-Oz` mapped to LLVM optimization pipelines via the new pass manager.
- Cross-compiles via target triples; LLVM does the heavy lifting.
- Emits debug info as DWARF (Linux/macOS) or CodeView (Windows) using LLVM's debug info builder, fed by per-LIR-instruction source spans.
- Uses **lld** as the linker by default for all targets (one linker, one set of bugs).

LLVM is not on the hot path for `arsenal codec` REPL or for `arsenal build --fast`; it is used for `arsenal build --release` and cross-compile scenarios where output quality matters more than compile latency.

### F.3 Backend Selection

| Mode | Backend |
|---|---|
| `arsenal codec` (REPL) | Fast backend, JIT |
| `arsenal run` | Fast backend, JIT |
| `arsenal build` (default) | Fast backend, AOT |
| `arsenal build --debug` | Fast backend, AOT, with full debug info |
| `arsenal build --release` | LLVM, `-O2` |
| `arsenal build --release-fast` | LLVM, `-O3` |
| `arsenal build --release-small` | LLVM, `-Oz` |
| `arsenal build --target X` (cross) | LLVM (until fast backend supports target) |

---

## Part G: Runtime

### G.1 Stdlib Structure ("philosophers")

The standard library is partitioned into MGS-philosophers-themed modules, each its own frequency:

| Module | Domain |
|---|---|
| `philosophers.mem` | Allocator cipher, allocator implementations, `mem.copy/set/eql` |
| `philosophers.io` | Buffered/unbuffered readers/writers, generic `Reader`/`Writer` ciphers |
| `philosophers.fs` | File system, paths, dir iteration |
| `philosophers.os` | Process, env, args, signals, exit |
| `philosophers.net` | TCP/UDP/Unix sockets, HTTP/1.1 client+server, TLS via vendored BoringSSL or rustls-equivalent |
| `philosophers.fox` | `fox` task spawn, channels, `nursery`, `await`, `lock` |
| `philosophers.fmt` | Print, format strings, `Formatter` cipher |
| `philosophers.codec` | JSON/TOML/binary serde via class metadata reflection |
| `philosophers.math` | Numeric, vector math, transcendentals |
| `philosophers.simd` | SIMD vector types, intrinsic wrappers |
| `philosophers.gfx` | Optional, Phase 10+: minimal rendering bindings (Vulkan/Metal) |
| `philosophers.test` | Test harness, assertions, golden-output utilities |
| `philosophers.collections` | Hash map, B-tree, dynamic array, ring buffer, bit-set |

### G.2 Allocators

The `Arsenal` cipher:

```gw
cipher Arsenal {
    fn alloc(self: &mut Self, layout: Layout) -> !AllocError [*]u8;
    fn realloc(self: &mut Self, ptr: [*]u8, old: Layout, new: Layout) -> !AllocError [*]u8;
    fn free(self: &mut Self, ptr: [*]u8, layout: Layout);
}
```

Implementations:

- **Heap** — wraps libc malloc, jemalloc, or mimalloc, selectable at link time. Default is mimalloc on Linux/Windows for performance, system malloc on macOS (Apple's malloc is excellent).
- **OuterHeaven** — bump arena. Allocates from a fixed buffer (or a chain of mmap'd pages); `free` is a no-op; entire arena released at once. Direct port of Zig's `ArenaAllocator`.
- **OuterHeavenVirtual** — reserves a large virtual region (default 1 TiB on 64-bit) via `mmap(MAP_NORESERVE)` / `VirtualAlloc(MEM_RESERVE)`, commits 64 KiB pages on demand. Grows without invalidating pointers.
- **Pool** — fixed-size object pool. Free list of slots in a single mmap'd region. O(1) alloc and free.
- **Tracking** — wraps another allocator; records allocation sites + sizes for leak detection. Used in tests.
- **Panic** — alloc and free panic. Used as the default for `#[no_alloc]` code.
- **FixedBuffer** — wraps a user-provided byte array; bump-allocates within.

Allocator passing: the Zig style (explicit `allocator: &Arsenal` parameter) is the **default**, because explicit is better than implicit. The Odin/Jai style (`context.allocator`) is supported via an opt-in `with_context` block that sets a thread-local context for legacy/ergonomic interop:

```gw
with_context (.allocator = my_arena) {
    let v: [dyn]i32 = .new();   // picks up context.allocator
}
```

### G.3 Fox Task Scheduler

**M:N work-stealing**, modeled on Tokio + Go scheduler hybrid:

- One OS thread per CPU core (configurable via `MotherBase.gw` or env).
- Each worker has a local LIFO bounded deque (Chase–Lev style; bound 256 tasks).
- A global FIFO injection queue for newly spawned tasks from non-worker threads.
- Idle workers steal a *batch* (half) from random victims.
- "Spinning thread" optimization (Go-style) keeps one worker hot to avoid park/unpark thrash.

**Stacks.** Two stack strategies, runtime-selectable per task type:

- **Fiber stacks** for `fox` tasks that may call into C / blocking code: 64 KiB initial, mmap-guarded; growable via a separate larger stack on overflow.
- **Stackless coroutines** for `async fn`: state-machine-transformed at compile time (rustc-style), no stack allocation.

The scheduler integrates with the I/O subsystem via a per-worker reactor; tasks blocked on I/O register a waker, are parked, and are resumed when the I/O subsystem reports completion.

### G.4 Channels (`codec_channel<T>`)

Bounded MPMC. Implementation:

- For capacity ≤ 1: pair of `AtomicU64` slots, one for value, one for signalling.
- For capacity > 1: array-backed Vyukov-style bounded MPMC ring buffer with sequence numbers per slot — lock-free producers and consumers, parking on full/empty via futexes (Linux), `WaitOnAddress` (Windows), `__ulock_wait` (macOS).
- Closing the channel sets a sentinel; pending send/recv return `error.ChannelClosed`.

Unbounded channels are deliberately *not* provided — backpressure is mandatory by language style.

### G.5 I/O Subsystem

Modeled on libxev (Mitchell Hashimoto's cross-platform proactor) and TigerBeetle's IO abstraction:

- **Linux**: `io_uring` first; fall back to `epoll` when `io_uring` unavailable (kernel < 5.6 or seccomp denies it).
- **macOS / *BSD**: `kqueue`; file I/O delegated to a thread pool because kqueue does not natively support async file I/O.
- **Windows**: `IOCP` with `ReadFileEx`/`WriteFileEx`/registered I/O for sockets.
- **WASM**: `poll_oneoff` via WASI.

The proactor model (completion-based) is the canonical API; readiness-based backends emulate it by issuing the syscall in userspace upon readiness notification. The unified API exposes:

```gw
io.read(handle, buf) -> !IoError usize
io.write(handle, buf) -> !IoError usize
io.accept(socket) -> !IoError Socket
io.connect(addr) -> !IoError Socket
io.timer(duration) -> !IoError Void
```

Each call is `await`-able from `async fn` or directly from `fox` task code.

### G.6 Panic & Error Return Traces

- **Panic**: prints message, captures stack trace via DWARF/PDB symbol resolution, calls user-set panic handler (default: `abort`).
- **Error return traces** (Zig-style): in debug builds, every `foxdie`-propagated error appends `(file, line, fn)` to a per-task ring buffer. On unhandled error, the trace prints the chain from origin to top-level. Cost is ~one cache line per error site; disabled in `--release`.

---

## Part H: Tooling

### H.1 `arsenal` Driver

Single binary. Subcommand dispatch via a generated table (entries declared via a `subcmd!` macro/`#virtuous` block). Subcommands:

- `arsenal build [path]` — build the project at `path` (default `.`).
- `arsenal run [path] -- [args]` — build and execute.
- `arsenal test [filter]` — discover and run tests; supports filter glob.
- `arsenal bench [filter]` — same for benchmarks; uses statistical harness (warmup, MAD outlier removal).
- `arsenal fmt [path]` — format files in place; `--check` for CI.
- `arsenal doc [path]` — generate documentation.
- `arsenal lsp` — start LSP server on stdio.
- `arsenal codec` — start REPL.
- `arsenal cipher add <pkg>` / `cipher install` / `cipher update` — package manager.
- `arsenal disasm <binary> [--symbol foo]` — disassembler with GW-aware annotations.
- `arsenal new <name>` — scaffold a project.
- `arsenal init` — initialize an existing directory.

### H.2 Formatter

AST-based pretty printer driven by the *Wadler-Lindig algebraic-pretty-printer* model (`Doc` algebra: text, line, nest, group, choice). Idempotency is verified by a fixed-point property test: `fmt(fmt(x)) == fmt(x)` for every file in the test corpus. Trivia (comments, blank lines) attach to AST nodes during parse and reattach during print using a documented heuristic (preceding comments belong to the following statement; trailing comments to the preceding statement).

### H.3 LSP Server

Built on `tower-lsp` (Rust phase) → port to GW post-bootstrap. Features:

- **Incremental parsing** via the CST: text changes are mapped to a token-range edit, the parser re-parses only the affected nodes (rust-analyzer's reparse strategy).
- **Error recovery** (already in the parser).
- **Hover** — synthesizes type, doc comment, attribute info.
- **Goto-def, find-refs, rename** — resolved via the symbol table; rename uses CST (preserving formatting).
- **Diagnostics** stream from the type checker and borrow checker.
- **Inlay hints** — types of inferred locals, monomorphized argument types.
- **Code actions** — quick-fix from diagnostic suggestions.

### H.4 Doc Generator

Walks each frequency, collects `///` comments and class metadata, renders to:
- **Markdown** for inline READMEs and IDE preview.
- **HTML** for static sites (single-page-app with client-side search, à la rustdoc).
- **JSON** for downstream tooling.

Class field metadata (`@range(0,100)`, `@serialize`) is rendered as structured tables. Examples in `///` are extracted and run as doctests.

### H.5 Package Manager (`cipher`)

`MotherBase.gw` is the manifest; resembles `Cargo.toml` semantically but written as GW source:

```gw
#virtuous {
    package = "snake-eater";
    version = "0.3.1";
    dependencies = .{
        "philosophers/net": .{ .version = "1.x" },
        "github.com/foxhound/decoy": .{ .git = "...", .rev = "..." },
    };
}
```

Lockfile (`MotherBase.lock`) records resolved versions and hashes (Blake3). Resolution uses the **PubGrub algorithm** (Dart's resolver, also used by uv/pip) for clear conflict explanations. Distribution: source tarballs over HTTPS, with a community-run registry; git URLs are first-class (Zig/Go style) for pre-registry development.

### H.6 REPL (`arsenal codec`)

The REPL *is* the compiler in JIT mode. Design:

- A persistent *session state*: symbol table, type table, JIT module, allocator arena.
- Each line is parsed; if it is a top-level declaration, it is added to the session and JIT-compiled; if it is an expression, it is wrapped in a synthetic function and invoked.
- `:load file.gw` includes a file (HolyC-style).
- `:type expr` prints the inferred type.
- `:disasm fn` prints the JITed machine code.
- `:save state.codec` / `:load state.codec` snapshot/restore the session (post-MVP).
- History persisted to `~/.arsenal/codec_history`.

JIT engine details in Part I.

---

## Part I: JIT Engine

### I.1 Architecture

The JIT engine sits behind the fast backend's emission interface. Instead of writing to an object-file buffer, the fast backend writes directly to a **JitMemoryRegion** — a managed RWX (or W^X-cycled) region.

### I.2 Incremental Emission

Each new top-level declaration in the REPL produces a new function. Functions are emitted to fresh page-aligned chunks of the JitMemoryRegion; the region grows by 64 KiB chunks on demand. Stale code (replaced by a redefinition) is left allocated until session end — simpler than a moving collector and cheap given REPL session sizes.

### I.3 W^X on Apple Silicon and Modern Linux

On Apple Silicon (mandatory W^X via `MAP_JIT`):

```gw
mmap(NULL, size, PROT_READ|PROT_WRITE|PROT_EXEC, MAP_PRIVATE|MAP_ANON|MAP_JIT, -1, 0);
pthread_jit_write_protect_np(0); // make writable
// ... emit bytes ...
pthread_jit_write_protect_np(1); // make executable
sys_icache_invalidate(addr, len);
```

The MAP_JIT pages are W^X per-thread; the toggle is fast (no syscall). Codesigned binaries must hold the `com.apple.security.cs.allow-jit` entitlement.

On Linux with hardened policies (e.g., SELinux deny_execmem):

```gw
// Two mappings of the same physfile-backed region: one RW, one RX.
fd = memfd_create("gw-jit", 0);
ftruncate(fd, size);
rw = mmap(NULL, size, PROT_READ|PROT_WRITE, MAP_SHARED, fd, 0);
rx = mmap(NULL, size, PROT_READ|PROT_EXEC,  MAP_SHARED, fd, 0);
// Write through rw, execute through rx; flush dcache before exec.
```

On Windows: `VirtualAlloc(... PAGE_READWRITE ...)` then `VirtualProtect(... PAGE_EXECUTE_READ ...)` with `FlushInstructionCache`. ARM64 Windows requires the same dance.

### I.4 Linkage Between JITed and AOT Code

The REPL session may import compiled crates (`philosophers.fmt`, etc.) loaded as ordinary shared libraries. JITed code calls into AOT code via plain function pointers resolved at parse time. Symbols from the main binary are exposed via `dlsym`-of-self (Linux/macOS) or `GetModuleHandle(NULL)` + `GetProcAddress` (Windows).

### I.5 REPL-Scope Symbol Resolution

The REPL maintains a "session frequency" — a virtual frequency containing all REPL-defined symbols. Lookup order: session → user-imported frequencies → philosophers stdlib. Redefining a symbol in the REPL shadows the older version (new MIR replaces old in the session frequency); existing JITed code that called the older version retains the old pointer until it is recompiled — a conscious simplification (no patchpoints in v1).

---

## Part J: Cross-compilation Infrastructure

### J.1 Bundled libc Matrix

Following Zig's lead, GW bundles the source of every supported libc:

| Target | libc | Provenance |
|---|---|---|
| Linux x86_64/aarch64/riscv64 musl | musl 1.2.x (vendored) | https://musl.libc.org |
| Linux x86_64/aarch64 glibc | headers for glibc 2.17 .. 2.40 (40+ versions) | upstream sources |
| macOS x86_64/aarch64 | macOS SDK headers (vendored selectively under Apple's licensing terms — same approach Zig takes) | Xcode SDK |
| Windows x86_64/aarch64 | mingw-w64 headers + Windows SDK headers (Zig's strategy) | mingw-w64 + MS SDK redistributables |
| Freestanding | none | n/a |
| WASM (WASI) | wasi-libc | https://github.com/WebAssembly/wasi-libc |

For Linux glibc: GW does not link glibc statically (glibc forbids it). Instead, GW emits stub shared libraries with the right symbol versions for the requested target glibc, and the actual glibc is dynamically linked at runtime on the target machine. Cross-compiling to Linux glibc 2.17 from any host is one flag: `--target x86_64-linux-gnu.2.17`.

### J.2 Per-target Stdlib

`philosophers.os` and `philosophers.io` have target-conditional implementations selected via `#virtuous` configuration switches reading the target triple. Most stdlib code is target-agnostic (uses the `Reader`/`Writer`/`Arsenal` ciphers).

### J.3 Linker

`lld` is bundled and invoked for every target by default:
- ELF: `ld.lld`
- Mach-O: `ld64.lld`
- COFF: `lld-link`
- WASM: `wasm-ld`

Users can override with `--linker=mold|ld|gold|...`. lld is chosen because it is fast, cross-platform, and well-maintained alongside LLVM; dropping system linker dependencies is the entire point of a cross-compiling toolchain.

---

## Part K: Testing & Validation

### K.1 Compiler Self-tests

Unit tests in the host language for every pass: lexer (token stream golden files), parser (CST shape), resolver (symbol resolution outcome), type checker (Hindley-Milner-style judgment validation), MIR builder (CFG shape), borrow checker (accept/reject corpora), backends (functional tests + assembly golden files).

### K.2 Language Tests: `snake_eater`

The `snake_eater` test suite is the language conformance corpus, organized as:

```
tests/snake_eater/
  pass/       — programs that must compile and produce expected stdout
  fail/       — programs that must produce specific diagnostics (matched by error code)
  bench/      — performance regression suite
  fuzz/       — corpus seeds for AFL-style fuzzing
```

Each `.gw` file may be paired with `.expected_stdout`, `.expected_stderr`, or `.expected_diagnostics` (a list of `error_code:line:col` triples).

### K.3 Differential Testing

For FFI boundaries: a corpus of identical programs in GW, C, and Zig that exercise struct layout, calling conventions, varargs. Differential test runs all three, asserts identical observable behavior. Tools: `csmith` for C; structurally-similar GW emitter for the GW side. This catches ABI bugs that pure unit tests miss.

### K.4 Fuzzing

- **Parser fuzzing**: AFL++ + libFuzzer harnesses on the parser entry point. Goal: no crashes, no infinite loops, only well-formed diagnostics.
- **Type checker fuzzing**: a grammar-aware fuzzer (tree-sitter-based) emits syntactically valid programs; type checker must not crash.
- **Comptime engine fuzzing**: grammar-aware fuzzer emits `#virtuous` blocks; the engine must respect operation/memory/recursion caps.
- **Borrow checker fuzzing**: corpus mining from `pass/` and `fail/` plus minimization on rejected programs (creduce-style).

### K.5 Compile-time Stress Tests

- Generic instantiation explosion: a program with a `[2^16]`-element type tuple recursively constructed.
- Comptime recursion at the cap (must produce a clean diagnostic).
- 100k-line synthetic program (csmith-derived) for the 1M LoC/s/core benchmark.
- 10k-function program with deep import graph for incremental cache validation.

### K.6 Continuous Integration Matrix

- Hosts: x86_64-linux-musl, x86_64-linux-gnu, x86_64-macos, aarch64-macos, x86_64-windows.
- Targets: full cross-product on every host.
- Bootstrap test: from the source bootstrap (C shim or WASM blob) to a working `arsenal`, every commit.
- Self-host fixed-point: `arsenal-2 == arsenal-3` byte-for-byte (post Phase 6).

---

## Part L: Implementation Roadmap (Phased)

Phases are sequenced, not timed. Each phase has entrance criteria, deliverables, exit criteria, risks/mitigations, and validation tests. Phases may overlap where dependencies permit.

### Phase 0 — Foundations

**Entrance criteria.** Repository created; host toolchain (Rust stable + LLVM 18 dev libs) installed in CI.

**Deliverables.**
- Repo skeleton: `compiler/`, `stdlib/`, `tools/`, `tests/`, `bench/`, `docs/`.
- `compiler/arsenal-boot/` Cargo workspace with crates: `arsenal_lex`, `arsenal_parse`, `arsenal_ast`, `arsenal_driver`.
- Lexer for the full GW token grammar.
- Parser for a *minimal* subset: top-level fns, classes (POD only), `let`, `if`, `while`, `return`, integer + bool + string literals, basic binary/unary operators.
- `arsenal new`, `arsenal build` (echoes ASTs to stdout for now), `arsenal --version`.
- CI pipeline: build + lint + unit-test on three OSes.

**Exit criteria.** `arsenal new hello && arsenal build hello` produces a (typed) AST dump; lexer/parser unit tests pass; CI green on Linux/macOS/Windows.

**Risks & mitigations.**
- *Risk*: under-specified grammar lets ambiguity creep in. *Mitigation*: write `docs/grammar.ebnf` first; update with every parser change.
- *Risk*: Rust LLVM crate version churn. *Mitigation*: pin `inkwell` and `llvm-sys` versions; vendor headers if necessary.

**Validation tests.** Snapshot tests of CST/AST output for a 50-file `snake_eater/pass/lexparse/` corpus.

### Phase 1 — Bootstrap Compiler (LLVM-only end-to-end)

**Entrance criteria.** Phase 0 complete.

**Deliverables.**
- Resolver and bidirectional type checker for non-generic code.
- MIR builder (no borrow check yet) and LLVM backend via `inkwell`.
- Stdlib: `philosophers.mem` (Heap allocator only), `philosophers.fmt` (Print + format), `philosophers.io` (stdout/stderr writer).
- Implicit Print of bare string literals.
- Top-level execution semantics (synthesize a `_start` from top-level statements).
- Hello World; basic class definitions; `if`/`while`/`for in range`/`break`/`continue`; integer arithmetic.
- Linker invocation via lld for native target.

**Exit criteria.** A 200-program `snake_eater/pass/phase1/` corpus runs, executable, produces correct stdout. Hello World binary runs on Linux x86_64, macOS arm64, Windows x86_64.

**Risks & mitigations.**
- *Risk*: getting LLVM's debug info correct. *Mitigation*: punt; emit stripped binaries in Phase 1, add DWARF in Phase 2.
- *Risk*: Windows toolchain (MSVC vs mingw). *Mitigation*: mingw-w64 first; MSVC CRT later.

**Validation tests.** End-to-end run-and-compare on the corpus; LLVM IR golden-file diff for a 20-program canary subset.

### Phase 2 — Generics, Ciphers, Sum Types, Comptime

**Entrance criteria.** Phase 1 complete.

**Deliverables.**
- Generic parsing and instantiation (monomorphization with caching).
- `cipher` declarations and `cipher T for U` impls.
- `liberty` sum types + exhaustive `switch` (compile error on missing arms).
- Tree-walking comptime engine (initial implementation).
- `#run`, basic `#virtuous { }`.
- `@codec(T)` returning a partial `TypeInfo`.
- DWARF debug info via LLVM.

**Exit criteria.** Generic `Vec<T>` and `Option<T>` work end-to-end; a `derive_debug` macro via `#run` + reflection works on POD classes; `gdb`/`lldb` show source-level locals.

**Risks & mitigations.**
- *Risk*: cipher resolution exponential in pathological cases. *Mitigation*: depth limit on selection (default 256); explicit error.
- *Risk*: tree-walking interpreter slow on heavy comptime. *Mitigation*: accept it for Phase 2; replace in Phase 5.

**Validation tests.** `snake_eater/pass/generics/`, `snake_eater/pass/liberty/`, `snake_eater/pass/comptime/` corpora.

### Phase 3 — Memory Safety

**Entrance criteria.** Phase 2 complete.

**Deliverables.**
- Region-based borrow checker (Part D.5).
- `defer` and `errdefer` lowering.
- All allocator implementations (Heap with mimalloc, OuterHeaven, OuterHeavenVirtual, Pool, Tracking, Panic, FixedBuffer).
- Three safety tiers (`safe` default, `manual`, `naked`).
- `?T` optionals with `if let` / `orelse` sugar.

**Exit criteria.** `snake_eater/fail/borrow/` rejects invalid programs with helpful diagnostics; allocator unit tests pass; `tracking` allocator detects leaks in synthetic tests.

**Risks & mitigations.**
- *Risk*: borrow checker false positives drive users to `manual` everywhere. *Mitigation*: corpus-driven design — collect rejection examples, prioritize NLL-equivalent precision.
- *Risk*: lifetime annotations on functions become viral and ugly. *Mitigation*: lifetime elision rules (Rust-style); function-local regions don't surface in signatures unless cross-borrow is explicit.

**Validation tests.** Borrow-check accept/reject corpora; allocator fuzz tests; valgrind/ASAN clean on stdlib test suite.

### Phase 4 — Error Handling & Concurrency

**Entrance criteria.** Phase 3 complete.

**Deliverables.**
- `!E T` error unions with set inference.
- `foxdie` propagation, `catch |e| ...` recovery.
- Error return traces in debug.
- `fox` task scheduler (M:N work-stealing).
- `codec_channel<T>` MPMC.
- `async fn` / `await` (state-machine transform on MIR).
- `nursery { }` structured concurrency.
- `lock { }` blocks (LOCK prefix preserved).
- `atomic[Order] T` typed atomics.
- I/O subsystem: io_uring / kqueue / IOCP backends.
- Auto-derived `Send`/`Sync`.

**Exit criteria.** A reference HTTP/1.1 echo server in pure GW handles 100k connections with < 100 MB RSS; `nursery` tests demonstrate scoped task cancellation.

**Risks & mitigations.**
- *Risk*: async state-machine transform is intricate. *Mitigation*: model on rustc's generator transform; extensive tests.
- *Risk*: scheduler starvation under burst load. *Mitigation*: cooperative yield insertion at backedges (Tokio's coop budget).
- *Risk*: I/O backend differences leak through abstraction. *Mitigation*: libxev as reference design; conformance test suite that runs identical I/O programs on every backend.

**Validation tests.** `snake_eater/pass/async/`; stress-test harness for scheduler; HTTP echo benchmark.

### Phase 5 — Self-hosting Preparation

**Entrance criteria.** Phase 4 complete.

**Deliverables.**
- Stack-VM comptime engine (replaces tree-walking).
- Full reflection API: `@field`, `@call`, `inline for`, attribute access.
- `rex { }` inline assembly with named-local visibility.
- `#[interrupt]`, `#[naked]` function attributes.
- `arsenal build --import-c foo.h`: invoke libclang to lower C headers to GW `extern class` + `extern fn` declarations (Zig `@cImport` style).
- `extern class` C-compatible layout rules.

**Exit criteria.** The "GW₀" subset is documented in `docs/gw0_subset.md`; a hand-translation feasibility study converts ~5% of the Rust compiler to GW₀ source for review; the comptime stack VM is ≥ 10× faster than tree-walking on the benchmark.

**Risks & mitigations.**
- *Risk*: libclang is a gigantic C++ dependency. *Mitigation*: dynamically load libclang at runtime (delay-load); ship without it as an optional component.
- *Risk*: `rex` block grammar accidentally encodes too much architectural state. *Mitigation*: per-arch validators that reject `rex` blocks referencing wrong-arch mnemonics with clear errors.

**Validation tests.** Comptime VM benchmark; `--import-c` against {libc, sqlite3.h, libcurl/curl.h}.

### Phase 6 — Self-hosting

**Entrance criteria.** Phase 5 complete; GW₀ subset frozen for ≥ 4 weeks of bug-fix-only.

**Deliverables.**
- `compiler/arsenal-self/` written in GW.
- Module-by-module port from `arsenal-boot` (Rust) to `arsenal-self` (GW). Order: lexer → parser → resolver → type checker → MIR builder → borrow checker → LIR → LLVM backend → driver → tooling.
- `bootstrap/` directory containing a frozen `arsenal-stage1.wasm` (compiled by the previous-stage compiler) for source bootstrap.
- Three-stage build (CMake or `arsenal-self`'s own build script):
  1. WASM blob → stage1 `arsenal` (interpreted/AOT-from-WASM).
  2. Stage1 compiles `arsenal-self` source → stage2 `arsenal`.
  3. Stage2 compiles `arsenal-self` source → stage3 `arsenal`.
- Acceptance: `sha256(stage2) == sha256(stage3)`.

**Exit criteria.** Fixed-point byte equality holds on all CI hosts; the full `snake_eater` corpus passes under `arsenal-self`; `arsenal-boot` is moved to `bootstrap/legacy/` and frozen.

**Risks & mitigations.**
- *Risk*: subtle codegen difference makes fixed point unreachable. *Mitigation*: deterministic codegen — sort hash map iteration where it affects output; stable symbol mangling; canonicalize SSA; same LLVM version.
- *Risk*: porting reveals semantic bugs in `arsenal-boot`. *Mitigation*: that's the point; fix forward; both compilers must stay in sync until the crossover.
- *Risk*: bootstrapping from source becomes onerous (multi-hour). *Mitigation*: WASM blob is small and pre-optimized; alternative bootstrap path via `mrustc`-of-GW (a tiny GW-to-C transpiler) is a research item.

**Validation tests.** Full snake_eater under stage2 and stage3; fixed-point hash check; cross-platform CI.

### Phase 7 — Fast Backend

**Entrance criteria.** Phase 6 complete (implementing TPDE in GW is more pleasant than in Rust).

**Deliverables.**
- `compiler/codegen_fast/`: TPDE-style template encoder for x86_64 + aarch64 targeting ELF + Mach-O + COFF.
- Direct in-memory emission for JIT mode.
- Linear-scan-with-spill register allocator.
- `arsenal codec` REPL using the JIT.
- W^X handling for Apple Silicon and hardened Linux.

**Exit criteria.** 1M LoC/s/core measured on a 100k-line csmith-generated benchmark on Zen 4 / M2 Pro; REPL latency < 100 ms for typical statements; functional parity with LLVM `-O0` on `snake_eater`.

**Risks & mitigations.**
- *Risk*: 1M LoC/s/core target unmet. *Mitigation*: it is aspirational. Fall back to "≥ 4× LLVM `-O0`" as a hard target. Profile: arena allocator, parser, RA likely the bottlenecks.
- *Risk*: aarch64 encoding bugs. *Mitigation*: differential test against Cranelift's encoder on a randomized instruction corpus.
- *Risk*: COFF/Mach-O object emission has corner cases. *Mitigation*: validate with `objdump`/`otool`; round-trip with `lld`.

**Validation tests.** 1M LoC/s benchmark; differential codegen against LLVM `-O0` for functional parity; `arsenal codec` smoke tests.

### Phase 8 — Cross-compilation

**Entrance criteria.** Phase 7 complete.

**Deliverables.**
- Bundled libcs (musl, glibc multi-version, mingw-w64, macOS SDK selectively, wasi-libc).
- lld integration for all object formats.
- `arsenal targets` lists supported `--target` triples.
- CI matrix: every host × every target.
- `philosophers.os` per-target implementations.

**Exit criteria.** Cross-compile `arsenal` itself from one host to every other host and run the binary on the target.

**Risks & mitigations.**
- *Risk*: macOS SDK redistribution has licensing constraints. *Mitigation*: follow Zig's precedent; ship only headers, not libraries; require Xcode for full macOS builds.
- *Risk*: glibc symbol versioning explosion. *Mitigation*: generate stub `.so` files at build time (Zig's strategy); keep the symbol-version table vendored.

**Validation tests.** Hello World and a TCP server cross-compiled in the full matrix; binaries exec'd on real targets in CI.

### Phase 9 — Tooling Maturity

**Entrance criteria.** Phase 8 complete.

**Deliverables.**
- LSP feature-complete: diagnostics, hover, goto, find-refs, rename, completion, inlay hints, code actions, semantic tokens.
- `arsenal fmt` idempotent; verified on the full stdlib + 1k-file random corpus.
- `arsenal doc` produces published documentation for `philosophers.*`.
- `cipher` package manager: registry, lockfile, semver resolution, integrity hashing.
- Markdown literate sources (`*.gw.md`): fenced GW blocks compile; surrounding markdown becomes documentation.
- `#asset(path)` for embedded binary data.

**Exit criteria.** A self-published `philosophers.net` package reachable via `arsenal cipher add`; rust-analyzer-grade IDE responsiveness; documentation site live.

**Risks & mitigations.**
- *Risk*: registry abuse / supply chain. *Mitigation*: cryptographic hashing in lockfile; signed releases; namespacing.
- *Risk*: LSP performance on huge projects. *Mitigation*: incremental CST + per-frequency cache from Phase 10.

**Validation tests.** LSP latency benchmarks; fmt idempotency; doc generator golden output.

### Phase 10 — Performance & Polish

**Entrance criteria.** Phase 9 complete.

**Deliverables.**
- Function-level incremental compilation (rustc-style red/green query DAG).
- Aggressive parallelism (per-function MIR construction, type checking, codegen).
- Optimization passes on MIR: const propagation, dead-code, inline-small, branch folding.
- Stdlib expansion: `philosophers.simd` (portable SIMD), `philosophers.gfx` (optional Vulkan/Metal bindings), expanded `philosophers.codec` (CBOR, msgpack), expanded `philosophers.net` (HTTP/2, WebSocket, TLS 1.3).
- ABI v1 freeze (see Part M).

**Exit criteria.** Edit-build-run on a 100k-line project < 500 ms median; full clean rebuild < 10 s on M-class hardware; stdlib feature list parity with Zig std and Rust std for application development.

**Risks & mitigations.**
- *Risk*: incremental compilation correctness regressions. *Mitigation*: red/green correctness fuzzing — random edits, incremental vs from-scratch output diff.

**Validation tests.** Incremental edit benchmarks; SIMD numeric tests against scalar reference; TLS interop with Rustls test vectors.

---

## Part M: Risk Analysis

### M.1 Borrow Checker Complexity Creep

**Risk.** Pressure to accept more programs (Polonius problem case #3, partial moves, self-borrows) leads to a Polonius-grade implementation that takes years and slows the compiler.

**Mitigation.** Hold the line at function-local NLL-precision. Document the pattern of false positives and the standard workarounds (restructure, `manual` block). Track upstream Polonius from a distance; adopt only after rustc ships and stabilizes.

### M.2 Comptime Engine Determinism and Performance

**Risk.** Hash-map iteration order, floating-point reductions, system time sneak into comptime via stdlib pathways and break determinism. Performance regresses as users push more logic into `#virtuous`.

**Mitigation.** Stdlib audit and `#[no_comptime]` attribute on every function that touches non-deterministic primitives. Hash maps used at comptime use a fixed seed and *insertion-ordered iteration* by default. Comptime CPU profile is a tracked metric; regressions block release.

### M.3 Fast Backend 1M LoC/s Target

**Risk.** Aspirational; may be unattainable across all source code styles.

**Mitigation.** Define the target precisely: csmith-generated, non-generic, `-O0` quality, single core, post-cache-warm. Publish honest numbers. Fall-back commitment: ≥ 4× LLVM `-O0` (TPDE achieves 8–24× on SPEC; 4× across realistic GW code is conservative).

### M.4 LLVM Dependency

**Risk.** LLVM is large, slow to build, and its API breaks every release.

**Mitigation.** `inkwell` abstracts most breakage. Pin to a tested LLVM version (initially LLVM 18; advance one major version per year). Maintain a Cranelift-backed fallback for development convenience. The fast backend (Phase 7) reduces LLVM exposure to release-only paths. **Do not** roll a custom optimizing backend — that path is a project of its own; the right answer is "use LLVM for release, fast backend for dev."

### M.5 ABI Stability

**Risk.** Decisions made early (struct field ordering, calling convention, vtable layout) become hard to change post-1.0.

**Mitigation.** Until Phase 10 there is no ABI stability promise — the language is `0.x` and breaking changes are routine. Phase 10 freezes ABI v1, documented in `docs/abi.md`. `extern class` always uses the C ABI of the target platform; that is the only ABI users should rely on for cross-language linkage.

### M.6 Self-hosting Cliff

**Risk.** The crossover (Phase 6) takes longer than expected, blocking the fast backend (Phase 7).

**Mitigation.** Crossover is gated by GW₀ subset stability, not feature completeness. Many Phase 9 tooling features (LSP, doc gen) can be developed in either compiler; do them in `arsenal-self` to amortize the port. If the cliff stretches, deliver an interim `arsenal codec` REPL using LLVM JIT (`inkwell`'s JIT API) as a stand-in for Phase 7 in `arsenal-boot`.

### M.7 Long-tail Platform Issues

**Risk.** Windows COFF/PE quirks, macOS Mach-O codesigning, Apple Silicon W^X, ARM64 codegen quality each consume disproportionate time.

**Mitigation.** Each platform has a designated owner-test in CI from day 1; bugs cannot accumulate. Prioritize: Linux x86_64 (P0) → Linux aarch64 (P0) → macOS aarch64 (P0) → macOS x86_64 (P1) → Windows x86_64 (P1) → Windows aarch64 (P2) → freestanding (P2) → WASM (P2) → riscv64 (P3).

### M.8 Async Runtime Model Risk

**Risk.** The chosen concurrency model (M:N + stackless async + structured nursery) is wrong for the dominant workload (server, embedded, game, scientific) and forces a rewrite.

**Mitigation.** All concurrency primitives are stdlib, not language. `fox`, channels, `nursery` can evolve with minor breakage; `async`/`await` and `Send`/`Sync` are language. The `lock { }` block is preserved from HolyC and is orthogonal. Provide a "no fox" mode (`philosophers.fox` excluded) for embedded/freestanding where users supply their own scheduling.

### M.9 Single-pass + Generics Tension

**Risk.** Single-pass-with-backpatching plays poorly with comptime-driven generic instantiation that reaches across files.

**Mitigation.** "Single pass" is a user-facing simplification (no header files); the implementation already does signatures-first then bodies. Comptime instantiation drives a worklist; this is well-understood.

### M.10 LSP Latency Under Realistic Load

**Risk.** Without function-level incrementality, LSP feels laggy on > 50k-line projects.

**Mitigation.** Per-frequency cache from Phase 1 already gives module-level incrementality; that is sufficient for early adopters. Phase 10's function-level red/green is the long-term answer.

---

## Part N: File and Repository Layout

### N.1 Top-Level

```
gw/
├── README.md
├── LICENSE                     (MIT/Apache-2.0 dual)
├── MotherBase.gw               (manifest for self-hosted compiler, post Phase 6)
├── bootstrap/
│   ├── arsenal-stage1.wasm     (frozen blob for source bootstrap, post Phase 6)
│   ├── bootstrap.c             (tiny WASM interpreter for full-source bootstrap)
│   └── legacy/                 (frozen Rust compiler source, post Phase 6)
├── compiler/
│   ├── arsenal-boot/           (Rust source, Phases 0–6)
│   │   ├── Cargo.toml
│   │   ├── crates/
│   │   │   ├── arsenal_lex/
│   │   │   ├── arsenal_parse/
│   │   │   ├── arsenal_ast/
│   │   │   ├── arsenal_resolve/
│   │   │   ├── arsenal_typeck/
│   │   │   ├── arsenal_mir/
│   │   │   ├── arsenal_borrow/
│   │   │   ├── arsenal_lir/
│   │   │   ├── arsenal_codegen_llvm/
│   │   │   ├── arsenal_codegen_fast/
│   │   │   ├── arsenal_comptime/
│   │   │   ├── arsenal_jit/
│   │   │   ├── arsenal_lsp/
│   │   │   ├── arsenal_fmt/
│   │   │   ├── arsenal_doc/
│   │   │   ├── arsenal_cipher/  (package manager)
│   │   │   └── arsenal_driver/  (entry point binary)
│   │   └── tests/
│   └── arsenal-self/           (GW source, Phase 6+)
│       ├── MotherBase.gw
│       └── frequencies/
│           ├── lex/
│           ├── parse/
│           ├── ast/
│           ├── resolve/
│           ├── typeck/
│           ├── mir/
│           ├── borrow/
│           ├── lir/
│           ├── codegen_llvm/
│           ├── codegen_fast/
│           ├── comptime/
│           ├── jit/
│           ├── lsp/
│           ├── fmt/
│           ├── doc/
│           ├── cipher/
│           └── driver/
├── stdlib/
│   └── philosophers/
│       ├── MotherBase.gw
│       ├── mem/
│       ├── io/
│       ├── fs/
│       ├── os/
│       ├── net/
│       ├── fox/
│       ├── fmt/
│       ├── codec/
│       ├── math/
│       ├── simd/
│       ├── gfx/
│       ├── test/
│       └── collections/
├── tools/
│   ├── snake_eater_runner/     (test corpus driver)
│   ├── csmith_gw/              (GW translator for differential testing)
│   ├── corpus_minimizer/       (creduce-style minimizer)
│   └── bench_harness/
├── tests/
│   ├── snake_eater/
│   │   ├── pass/
│   │   ├── fail/
│   │   ├── bench/
│   │   └── fuzz/
│   └── differential/
├── bench/
│   ├── compile_throughput/      (1M LoC/s suite)
│   ├── runtime/
│   └── memory/
├── libc-bundle/
│   ├── musl/
│   ├── glibc-headers/          (per-version symlink farm)
│   ├── mingw-w64/
│   ├── macos-sdk-headers/
│   └── wasi-libc/
├── docs/
│   ├── grammar.ebnf
│   ├── language-reference.md
│   ├── abi.md
│   ├── gw0_subset.md
│   ├── architecture.md         (this document)
│   ├── borrow-checker.md
│   ├── comptime.md
│   ├── stdlib/                 (per-frequency reference, generated)
│   └── tutorials/
└── ci/
    ├── github-actions/  (or codeberg/woodpecker, mirroring Zig's migration)
    └── matrix.yaml
```

### N.2 Naming Conventions

- **Source files**: `snake_case.gw`. Modules are directories; the directory name is the module name.
- **Markdown literate sources**: `topic_name.gw.md`. Fenced ```gw blocks are extracted and compiled in source order.
- **Class names**: `PascalCase` (`OuterHeavenAllocator`, `CodecChannel`).
- **Function names, variables, fields**: `snake_case`.
- **Constants**: `SCREAMING_SNAKE_CASE`.
- **Cipher names**: `PascalCase` (`Arsenal`, `Reader`, `Iterator`).
- **Liberty variants**: `PascalCase`.
- **Compiler attributes**: `#[snake_case]`.
- **Comptime intrinsics**: `@snake_case` (`@codec`, `@field`, `@call`).

### N.3 Build Flow

**Phase 0–6 (Rust host):**
```
cargo build --release -p arsenal_driver        # builds arsenal-boot/arsenal binary
arsenal build stdlib/philosophers              # compile stdlib with bootstrap compiler
arsenal test tests/snake_eater                 # run conformance tests
```

**Phase 6+ (self-hosted):**
```
# From WASM blob source bootstrap:
cc bootstrap/bootstrap.c -o stage0
./stage0 bootstrap/arsenal-stage1.wasm compiler/arsenal-self -o stage1
./stage1 build compiler/arsenal-self -o stage2
./stage2 build compiler/arsenal-self -o stage3
sha256sum stage2 stage3                        # must match

# From previous-release binary (faster CI):
arsenal-prev build compiler/arsenal-self -o stage1
./stage1 build compiler/arsenal-self -o stage2
sha256sum stage1 stage2                        # fixed-point check
```

The self-hosted build is driven by a `MotherBase.gw` script à la Jai/Zig (no make/cmake on the user side). The compiler invokes `philosophers.os` to spawn lld; everything else is pure GW.

---

## Closing Synthesis

GW's design draws coherently from four traditions: HolyC's interactive single-pass shell-as-compiler, Zig's comptime + bundled-libc cross-compilation + arena allocators, Odin's pragmatic context system + simple scope-based memory model, and Rust's borrow-checked safety + cipher-style traits + ecosystem maturity. The implementation strategy mirrors that pragmatism: a Rust scaffold accelerates the front end and stdlib, a TPDE-inspired fast backend delivers HolyC-grade interactive responsiveness, an LLVM backend ensures release-quality codegen and cross-compilation, and the self-hosting handoff at Phase 6 occurs *before* the most pleasant-to-write compiler component (the fast backend) is built — so that component is written in GW itself, dogfooding its inline assembly and comptime features. The phased roadmap is ordered to minimize rework: every artifact built in Phase N is a tested foundation for Phase N+1, and the only Phase 6 cliff is bounded by an explicit, documented language subset rather than by the entire surface area of GW.

The sole significant aspirational target — 1M LoC/s/core in the fast backend — is grounded in TPDE's measured 8–24× speedup over LLVM `-O0`, and Jai's reported numbers in the same range. The phrasing throughout this document treats it as a target, not a guarantee, with a documented fallback (≥ 4× LLVM `-O0`).

The document is intended as the technical contract between language design and language implementation. It is dense by design; subsequent specifications (`docs/grammar.ebnf`, `docs/abi.md`, `docs/borrow-checker.md`, `docs/comptime.md`) will deepen each section to the line-of-code level as each phase begins.