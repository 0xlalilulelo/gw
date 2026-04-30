# From the Temple to the Tanker: A Technical Analysis of HolyC and the Specification of **GW**, a Metal-Gear-Inspired Systems Language

---

## Part 1 — Deep Technical Analysis of HolyC

HolyC (originally named *C+*) is the just-in-time-compiled, statically typed, imperative programming language developed by Terry A. Davis between approximately 2003 and 2013 as the simultaneous kernel-implementation language, application language, and shell of TempleOS. It is best understood as *C with a small set of carefully chosen extensions, married to a single-pass JIT compiler that doubles as a command-line REPL running entirely in ring 0 on a 64-bit x86 machine with no virtual memory and no privilege separation*. The following analysis covers its grammar, type system, runtime, and toolchain in depth.

### 1.1 Lexical and Syntactic Differences from C

HolyC reuses C's whitespace-insensitive grammar, but it deletes, replaces, and adds a number of constructs:

- **No required `main()`.** Top-level statements outside any function are executed top-to-bottom as the file is compiled. This means a `.HC` file is simultaneously a program *and* a script.
- **Zero-argument calls without parentheses.** `Dir;`, `Dir();`, and `Dir("*");` are all legal call-sites; the bare-identifier form is ergonomically how command-line invocations are written.
- **String literals as implicit `Print()`** — a bare string literal at statement position is sugar for `Print(...)` with the same arguments. A bare multi-character literal in single quotes (`'\n';`) is sugar for `PutChars()`.
- **Default arguments anywhere in the parameter list**, not only at the end. `U0 Test(I64 i=4, I64 j, I64 k=5)` is legal, with call-site holes such as `Test(,3);` to skip defaults.
- **Address-of required for function pointers.** `&MyFn` is mandatory; bare `MyFn` is always a call.
- **Postfix type casts.** `x(F64)` is a cast; there are also intrinsics `ToI64`, `ToBool`, `ToF64`.
- **Chained relational operators.** `5 < i < j+1 < 20` desugars to the expected conjunction.
- **No ternary operator** (`?:` is removed).
- **No `continue`** (Davis recommends `goto`).
- **No `#define`** macros and **no `typedef`** — `class` replaces both struct definitions and named type aliases.
- **`#include` requires `""`** form (no angle brackets); it is logically a textual concatenation, not a separate translation unit.
- **`#exe { ... }`** — a compile-time *escape hatch*. A `#exe` block executes arbitrary HolyC code at compile time and that code may emit text via `StreamPrint()` directly into the source token stream. This is HolyC's macro system, generic system, and code-generation system rolled into one: it's effectively unrestricted comptime done by re-entering the compiler.
- **`$...$` escapes** mark DolDoc rich-text commands, sprite tags, links, and color directives embedded directly in the source file.
- **Single-quote multi-byte literals.** `'ABC'` is the 8-byte little-endian quad `0x434241`. `PutChars` will emit that quad to the screen.
- **Switch statements** are jump-table-only by construction. They support implicit case numbering (a bare `case:` increments from the previous), case ranges (`case 4...7:`), unchecked variants (`switch [x]` skips bounds checks), and nested *sub-switch* groups delimited by `start:`/`end:` labels.

### 1.2 Type System

HolyC's primitive type set is small and explicit:

| Type | Meaning |
|---|---|
| `U0` | void, but with **size 0** (unlike C, where `sizeof(void)`==1 in GCC) |
| `I8`, `U8` | 1-byte signed/unsigned (also serves as `char`) |
| `I16`/`U16` | 2-byte |
| `I32`/`U32` | 4-byte |
| `I64`/`U64` | 8-byte (the natural register width) |
| `F64` | 8-byte IEEE-754 double — the **only** floating type |

Notable properties:

- **All values are register-promoted to 64 bits** on access; intermediate arithmetic happens in 64-bit registers. Signed-vs-unsigned right-shifts therefore return values consistent with the *register*, not with the declared variable width.
- **No type checking.** The compiler does not reject type-incompatible assignments; the language relies on programmer discipline.
- **No `typedef`.** All compound types are introduced via `class`, which is closer to a C++ POD-struct than to a C++ class. There are no methods in the C++ sense; only fields plus optional metadata.
- **Single inheritance only.** A subclass syntactically prefixes its parent: `class Derived:Base { ... };`.
- **Unions are typed.** Placing a primitive in front of a `union`/`class` declaration declares "when used as a scalar, this union has this primitive type." The classic example is `public I64i union I64 { I8i i8[8]; U8i u8[8]; ... }` which lets you index `i.u16[1]` directly on an integer.
- **Field metadata.** Class fields can be annotated with arbitrary metadata pairs (e.g. `format`, `data`, plus user-defined keys) which are preserved by the compiler and queryable at runtime via the compiler's symbol table — this gives HolyC a primitive form of compile-time + runtime reflection. See `DocForm()`.
- **`offset(class.field)`** and `sizeof(...)` work as compile-time intrinsics (one level deep).
- **`lastclass`** — a special default-argument keyword that resolves to a string of the type-name of the *previous* parameter, used for type-driven dispatch and reflective form generation.

### 1.3 Functions, Variadics, Defaults

- Variadic functions use `(...)` and access arguments via implicit `I64 argc` and `I64 argv[]` — every argument is promoted to 64 bits, regardless of source type. There is `StrPrintJoin(NULL, fmt, argc, argv)` to forward variadics into a `printf`-shaped callee, including for heap-allocated formatted strings.
- **Function flags** (placed in the prototype) are first-class: `interrupt` (ISR entry/exit), `haserrcode`, `public`, `argpop`/`noargpop` (control C ABI's stack-popping behaviour). The `interrupt` flag is what makes IRQ handlers like `IRQKbd()` writable as ordinary HolyC.
- **Register-pinned locals.** A local can be declared `reg R15 i = 5` to pin it to a hardware register, or `noreg j` to force it to the stack. This is essential for the *interleaved inline assembly* style HolyC uses pervasively.

### 1.4 Memory Model and Runtime

- **Single address space, ring 0, no virtual memory.** Every task shares the entire 64-bit address space. There is no page-table-enforced isolation; a wild pointer trashes the system.
- **Per-task heaps.** Each task has its own heap. `MAlloc()`/`Free()` operate on the *current* task's heap by default but can be redirected to another task's heap. `HeapCtrlInit()` builds an independent heap. `MSize()` returns the actual rounded-up allocation size (large allocations round up to the next power of two, and the slack is reachable).
- **The stack does not grow.** Because virtual memory is absent, page-fault-driven stack growth is impossible. `MEM_DFT_STK` is set at task spawn; large locals must be heap-allocated explicitly. `CallStkGrow()` exists but is awkward.
- **`Free(NULL)` is legal** (matches C).
- **`lock { ... }` blocks** apply x86 `LOCK` prefixes for atomic read-modify-write across cores. SMP is supported (TempleOS is multitasking on up to 64 cores) but synchronisation is otherwise raw.
- **No GC**, no RAII, no lifetimes, no destructors. Manual `MAlloc`/`Free` discipline.

### 1.5 Compilation Model

HolyC is, in the original Davis implementation, a **single-pass, line-at-a-time JIT** that emits x86-64 directly. There is no separate AST or optimiser worth speaking of in the upstream compiler — the compiler reads tokens, generates code, and patches forward references as needed. Forward-reference resolution is handled via a deferred symbol-fixup pass within the same compile.

Concretely:

1. The user types `Foo;` at the shell. The shell *is* the compiler.
2. The compiler tokenises, generates code into a freshly allocated executable buffer, performs symbol fix-ups, and jumps to it.
3. Top-level statements thus run immediately.
4. Function definitions install symbols into the task's symbol table; subsequent calls bind to those symbols.

Because of this, **there are no header files** and no separate-compilation / linker phase. Cross-file declarations are handled by `#include` (textual) and the `import`/`_extern`/`extern`/`_import` linkage keywords. `_extern SYMBOL T fn(...)` declares an extern with a known assembly label and avoids the textual include. Compilation speed is reported as fast enough that the entire OS plus all included applications recompile from source in a matter of seconds on early-2010s hardware.

### 1.6 The Shell / REPL Identity

In TempleOS, *the shell is the compiler*. `Dir;` is not a built-in shell command; it is a HolyC function call compiled and executed live. `5 + 7` at the prompt is an expression that is compiled to code that prints its value. There is no separation between "interactive" and "compiled" modes; therefore HolyC has no separate REPL eval semantics — interactive code shares the global namespace and types of every other compiled function in the system. This is structurally identical to a Common Lisp image but realised in a low-level imperative language.

### 1.7 DolDoc — Rich-Text Source

Source files are stored in **DolDoc**, a hypertext format akin to a stripped-down RTF. A `.HC` file may contain:

- inline 16-color sprites (drawn with `Ctrl-R`),
- 3-D meshes,
- hyperlinked file/grep/manual references (the `LK` directive),
- macros — `$MA,...$` — that execute HolyC when clicked,
- form widgets, trees, anchors, and per-character color/blink/font attributes,
- inserted-binary references (`$IB,...$`) that splice sprite data into the compiled stream as a string constant for use by `Sprite()`/`Sprite3()`.

DolDoc unifies what other systems split between source code, documentation, shell scripts, manpages, HTML, and config files. `Ctrl-T` toggles raw vs. rendered view. Filenames printed in the shell are themselves DolDoc hyperlinks.

### 1.8 Inline Assembly

Inline `asm { ... }` blocks use a TempleOS-specific assembler (non-standard mnemonics — e.g. `IMUL` and `IMUL2` are split to make the encoder simpler). Local variables are visible by name; the `IMPORT` directive within asm pulls a HolyC variable into asm scope. Top-level (file-scope) `asm` blocks are *skipped over* by the compiler at run-time — i.e. they assemble and emit code but execution falls through them. A function may be implemented purely as labelled `asm`, exposed via `LABEL::` and called by `Call(LABEL)`.

### 1.9 Standard Library / Built-ins

HolyC ships, as part of the OS, a sprawling library exposed as ordinary HolyC functions: `Print`, `PutChars`, `MAlloc`, `Free`, `Spawn`, `Sleep`, `Dir`, `FileRead`, graphics primitives (`GrPlot`, `GrLine`, `GrPrint`, `Sprite`, `Sprite3` for 3-D meshes), MIDI/PC-speaker tones (`Play`, `Snd`), the IRQ table accessors, the heap controller, and DolDoc rendering primitives. Because the library lives in the same single image as the user code, calling it has zero ABI cost.

### 1.10 Public/Private and Linkage

`public` is a keyword applied to function and class declarations to expose them in the global symbol table; without it, names are file-private. `extern`/`_extern` declare prototypes referring elsewhere; `import`/`_import` similarly mediate cross-translation-unit references. Because compilation is single-pass and cross-file forward references are resolved by the loader, linkage is more ad-hoc and dynamic than in C.

### 1.11 Exception Model

`try { } catch { } throw "msg"` exists. `throw` accepts a single ≤ 8-byte multi-character literal (so it fits in a register), reachable from inside `catch` as `Fs->except_ch`. `Fs->catch_except = TRUE` terminates the search for a handler. `PutExcept()` is a default printer. There is no exception-object hierarchy; throws are single-token tags.

---

## Part 2 — Comparison with Modern Systems Languages

The matrix below summarises the properties most relevant to a HolyC-inspired language design. Each language is then discussed briefly. (Where a language is in active design, the values reflect the publicly documented state as of early 2026.)

| Property | **C (C23)** | **C++ (23)** | **Rust** | **Zig (0.16)** | **D** | **Nim 2** | **Odin** | **Jai** (closed-beta) |
|---|---|---|---|---|---|---|---|---|
| Memory safety | manual | RAII + unsafe | borrow-checker (safe by default) | manual + runtime checks | GC default; `@nogc`/`-betterC` | ARC/ORC default | manual + arenas/context | manual + #scope |
| Compile speed | fast | very slow (templates) | slow (LLVM + borrow ck) | fast (self-hosted) | fast (DMD) / slow (LDC) | fast (C backend) | fast | aimed at 1 M LoC/s; ~80 k LoC < 1 s in demos |
| Generics / metaprog. | none | templates + concepts | traits + generics | `comptime` (run code at CT) | templates + CTFE + mixins | macros + templates + CTFE | `proc` polymorphism + `where` | `#run`, `#insert`, polymorphic procs |
| Error handling | return codes / errno | exceptions | `Result`/`Option`, `?` | error union `!T`, `try` | exceptions / nogc nothrow | exceptions, `Result` | multi-return + Maybe | multi-return + context |
| Concurrency | pthreads | std::thread, atomics, coroutines | Send/Sync, async | async (in flux), threads | std.parallelism, fibers | `async`, `spawn` | threads, sync prims | Thread module + groups |
| Tooling | mature | mature | excellent (cargo) | excellent (`zig build`) | dub | nimble | `odin` driver | bundled |
| Sum types | tagged unions (manual) | `std::variant` (clunky) | `enum` (true ADTs) | tagged `union(enum)` | tagged unions via `std.sumtype` | `object variant`, `case` | `union` (tagged) | `union` (tagged) |
| Traits/interfaces | none | concepts + dyn | traits (static + dyn) | duck-typed via comptime | interfaces | concepts + multimethods | parametric polymorphism (no traits) | interfaces (limited) |
| Ownership model | none | RAII + move | affine + lifetimes | manual | GC / scope | ARC + isolated heaps | manual + arenas | manual |
| Allocator strategy | global libc | global new/delete | global by default; `Allocator` API maturing | **explicit allocator parameter** | GC default | shared GC | **implicit `context.allocator`** | implicit context allocator |
| Reflection | minimal | limited (RTTI, C++26 reflection) | minimal | full via `@typeInfo` at comptime | full via `__traits` | full via macros | full via `core:reflect` | first-class compile- and run-time |
| C ABI / FFI | native | native | extern "C" | imports `.h` directly | extern (C) + ImportC | importc | foreign import | bindings + #foreign |
| ABI stability | yes | yes (per-platform) | unstable Rust ABI | unstable | partially | not stable | not stable | not stable |
| Cross-compilation | depends | depends | excellent | **best-in-class** (built-in) | yes | yes | yes | planned |

### 2.1 Per-Language Notes

**C** remains the lingua franca and the ABI baseline; it has no generics, weak metaprogramming (textual macros only), no real error handling, no sum types, and no ownership model — but its simplicity, single-pass-friendly grammar, and ubiquity are exactly the features HolyC inherited. C is the design floor below which a systems language cannot fall without ceasing to be a systems language.

**C++** offers RAII, exceptions, templates, and an enormous standard library, but template instantiation and header inclusion produce the worst compile times of any language under consideration; the language has accreted overlapping features over four decades, and modern C++ requires expert-level knowledge of move semantics, concepts, ranges, and coroutines just to write idiomatic code.

**Rust** is the safety standard-bearer: the borrow checker statically prohibits use-after-free, double-free, and most data races (via `Send`/`Sync`). Compile times are slow (LLVM backend plus borrow analysis); ergonomic friction with cyclic data structures, callbacks, and self-referential types is well documented; the macro system (declarative `macro_rules!` plus procedural macros) is powerful but opaque. `Result<T,E>` / `Option<T>` plus the `?` operator give clean error propagation. Cargo's package management and tooling are arguably the strongest in the systems space.

**Zig** is the closest spiritual cousin to HolyC's "transparent C-with-superpowers" ethos. Its key innovations are:

- **Comptime** — arbitrary Zig code runs at compile time using the same syntax and semantics as runtime code; this single mechanism subsumes C++'s templates and macros and Jai's `#run`/`#insert`.
- **Allocator-as-parameter** — every standard-library function that allocates takes a `std.mem.Allocator` explicitly. No hidden allocations.
- **Error unions** — `!T` is sugar for `error{...} || T`; `try` propagates, `catch` recovers, error sets are inferred and statically checked.
- **No hidden control flow** — no operator overloading, no destructors, no exceptions; `defer` for cleanup.
- Built-in cross-compilation for any LLVM target, including bundling Clang as a C compiler so Zig is also a drop-in C cross-compiler.

Zig's weakness is that it explicitly *does not* aim for memory safety; null pointers and use-after-free remain possible (the runtime `GeneralPurposeAllocator` and address sanitizers help in debug).

**D** has a large feature set: classes, templates, mixins, compile-time function evaluation (CTFE), `__traits`, and a GC that pervades the stdlib. The `-betterC` mode disables druntime and the GC and trims D back to a usable subset for systems work, but it severs access to most of `std.*`. ImportC lets `dmd` parse `.c` files directly. D is conceptually closest to a "C++ that compiles fast and has CTFE."

**Nim** is a Python-inspired language with mature ARC/ORC reference counting (default in 2.0), a powerful AST macro system that is genuinely hygienic, an effect system, and a C/C++/JS triple backend. Its concision and metaprogramming are exceptional; its weaknesses are name visibility quirks (case-and-underscore-insensitive identifiers), a smaller ecosystem, and some impedance with manual-allocation idioms.

**Odin** (Ginger Bill) is a self-described "Pascal of the modern era": no preprocessor, no header files, packages instead of modules, an **implicit `context` value** carried on every Odin-calling-convention call that holds the current `allocator`, `temp_allocator`, `logger`, etc. This means you can override an entire library's allocation strategy at the call site without parameter plumbing — a practical answer to Zig's allocator-everywhere ergonomics. Odin has built-in matrix/vector/quaternion types, SoA support, parametric polymorphism, and `or_return`/`or_else` for error multi-returns.

**Jai** (Jonathan Blow) remains closed-beta in early 2026 but its public design is well-documented. Its calling cards are:

- **Fully arbitrary compile-time execution** via `#run`; the build system itself is Jai code with read-write access to the AST.
- **`#insert`** for textual code-injection metaprogramming.
- **A `context` value** (Odin's was inspired by it) holding allocator and ambient state.
- A **target compile speed of 1 million lines per second**; demos show 80 k-LOC builds in under one second on the x64 backend — radically faster than C++ or Rust.
- First-class **runtime + compile-time reflection**.
- Data-oriented defaults (SoA layouts, polymorphic procedures, slicing).

Jai is the spiritual successor to HolyC's "compile so fast that the compiler can be the shell" idea, generalised to a much richer language.

---

## Part 3 — HolyC's Weaknesses and Gaps

Stripped of its TempleOS context, HolyC is unsuitable for general-purpose work for the following technical reasons:

1. **No memory safety of any kind.** No bounds checks, no null checks, no UAF detection. Combined with ring-0 execution and a single address space, a single bad pointer halts the OS.
2. **No type checking.** The compiler does not reject mismatched assignments; the entire language relies on the programmer's eye.
3. **No Unicode.** Strings are 8-bit (effectively ASCII / CP-437); the editor doesn't render UTF-8 and the standard library has no notion of code points or grapheme clusters.
4. **Ring-0-only, single-address-space runtime.** There is no notion of a user-mode runtime, no separation between processes, no virtual memory, no MMU-backed stack growth. Porting HolyC off TempleOS (as projects like *holyc-lang* and *HolyCC2* have done) requires re-inventing a runtime.
5. **x86-64 only.** The compiler emits x86-64 directly with no IR; ARM, RISC-V, WebAssembly, and 32-bit are unsupported.
6. **No cross-compilation.**
7. **No package manager, no module system in the modern sense.** `#include` is textual and `import`/`_extern` are linkage keywords, not namespace mechanisms.
8. **No generics, traits, or concepts.** `#exe { }` is the only escape hatch and is essentially unhygienic textual codegen.
9. **No sum types / tagged unions** — all unions are untagged.
10. **No `Option`/`Result` and a degenerate exception system** that can carry only an 8-byte tag.
11. **No first-class concurrency primitives.** SMP works via raw `lock { }` blocks; there is no channel, atomic API, mutex hierarchy, or async runtime.
12. **No modern reflection runtime** — only the per-class metadata table.
13. **No formatter, linter, debugger integration, or LSP.** The OS *is* the IDE.
14. **A non-portable rich-text source format** (DolDoc) that no other tool can read or produce.
15. **No standard build system, test framework, or dependency manager.**
16. **Numerous footguns** baked into the language: unchecked casts; `goto` instead of `continue`; multi-byte char literals (`'ABC'`) silently producing arbitrary integer values; default-arg holes; switch statements always producing jump tables (catastrophic with sparse cases); register-pinned locals that can be clobbered if the wrong register is chosen.

---

## Part 4 — HolyC's Functional Strengths Worth Preserving

Independent of its theological framing, HolyC contains several genuinely worthwhile design ideas that have largely been overlooked by mainstream language design:

1. **Single-pass compilation that is fast enough to be the shell.** This is a *radical* design choice — it collapses the edit / compile / link / run / repl loop into a single instruction. Jai chases this; Common Lisp lives it; HolyC achieved it in a low-level imperative language.
2. **No headers, no separate translation units.** A program is its source tree; the compiler reads it linearly and resolves forwards as it goes. This eliminates a *huge* category of C/C++ build complexity.
3. **Implicit `Print`/`PutChars` of bare literals** — turning the language into an effortless command-line calculator and quick-output shell. This is ergonomically superior to `printf("...\n");` for interactive use.
4. **No required `main()`** — top-level statements execute as written, which is how scripting languages have always worked but systems languages almost never do.
5. **Default arguments anywhere in the parameter list, with named-hole call sites.**
6. **Direct hardware access** — `interrupt` functions, `lock` blocks, register-pinned locals, and ergonomic inline assembly with HolyC variables visible by name in asm scope.
7. **`#exe { }`** — pre-Jai compile-time arbitrary code execution that emits source text, decades before mainstream languages caught up.
8. **DolDoc**: rich-text in source code as a *first-class concept*. The 2020s rediscovery of "literate" code (Jupyter, Observable, Quarto, Marimo) validates the idea; HolyC was there in 2003. The mistake was the format, not the goal.
9. **Per-class metadata fields** that survive compilation and are queryable at runtime — a primitive but real reflection system.
10. **`lastclass`** — a default-argument keyword that resolves to the type-name of the previous parameter. A surprising and useful piece of compile-time introspection that no other systems language has.

A modern language that captured these properties without HolyC's systemic weaknesses would be genuinely novel.

---

## Part 5 — Specification: **GW**, A Systems Programming Language

### 5.1 Name and Naming Convention

The language is named **GW** (pronounced "gee-double-yoo"). In *Metal Gear Solid 2: Sons of Liberty*, GW is the optic-neural Patriot AI, designed by Emma Emmerich, that runs aboard *Arsenal Gear* and arbitrates information across the network — a coordinator, censor, and translator of the data-stream. The metaphor is exact: a compiler/runtime is the optic-neural system that mediates between programmer intent and the machine's tactical network.

The toolchain naming is consistent and thematic, but every name maps to a real technical artefact (the names are mnemonic, not flavour):

| Component | GW name | What it actually is |
|---|---|---|
| Compiler driver | `arsenal` | Single binary: compile, run, REPL, package, format, doc |
| Build / package manager | `codec` | Manifest format + lockfile + frequency-resolution |
| Module / package | *frequency* | Versioned, semver-tagged unit of code |
| Source root | *Mother Base* | Top-level project directory (`MotherBase.gw`) |
| Standard library | `philosophers` | The original Patriots — the foundational layer |
| REPL / live JIT shell | `codec` (interactive) | HolyC-style shell that *is* the compiler |
| Allocator interface | `Arsenal` | Trait providing `alloc`/`free`/`resize` |
| Arena allocator | `OuterHeaven` | Bump arena with bulk free |
| Compile-time region | `#virtuous { ... }` | "Virtuous Mission" — everything inside runs at compile time |
| Inline assembly | `rex { ... }` | Heavy-metal block; raw ISA access |
| Sum / tagged union | `liberty` | "Sons of Liberty" — multiple identities, one parent |
| Generic parameter | `[T: ADAM]` | `ADAM` is the *trait* root (the prototype-DNA project) |
| Error union | `!T` (keyword: `foxdie`) | Targeted, tagged failure of a code path |
| Trait / interface | `cipher` | Cross-organisation contract |
| Optional type | `?T` | Same as Zig/Odin |
| Async / task | `fox` | Lightweight task |
| Concurrency channel | `codec_channel` | Typed bounded MPMC channel |
| Test block | `snake_eater { }` | Built-in test runner |
| Benchmark block | `virtuous_mission { }` | Built-in compile-time / runtime benchmark |
| Unsafe block | `naked { }` | Strips safety; raw memory + arbitrary casts |
| Reflection intrinsic | `@codec(T)` | Returns a `TypeInfo` value |
| Ambient context object | `context` | Borrowed from Odin/Jai; carries allocator, logger, RNG |
| ABI lock | `cipher "C" fn ...` | C ABI extern |

The naming serves as a memory aid (each name evokes its actual semantic role) and a deliberate cultural signal away from HolyC's theological framing toward a tactical, espionage-fiction frame. Importantly **none of the names are required by the language**: every directive (`#virtuous`, `naked`, `liberty`, etc.) has a plain English alias (`#comptime`, `unsafe`, `enum union`) so a user who finds the theme distracting can write standard-form GW.

### 5.2 Design Goals and Philosophy

GW is designed against a small set of explicit goals, in priority order:

1. **Compile speed must support a JIT-as-shell** — the single most important property HolyC pioneered. Goal: 1 M LoC/s on a single core, mirroring Jai's target. This dictates everything below.
2. **No headers, no preprocessor, no separate compilation.** A program is a directed graph of source files discovered from `MotherBase.gw`.
3. **Explicit memory, optional safety.** Memory safety is a *gradient*, not a mode: safe-by-default with opt-out `naked` blocks for the kernel/embedded layer.
4. **One mechanism for metaprogramming**: `#virtuous` compile-time execution, used for generics, conditional compilation, build scripts, and codegen — Zig/Jai-style.
5. **Errors as values**, not exceptions; tagged unions and error sets are first-class.
6. **C ABI is the lingua franca.** GW links *to* and *from* C with no shim layer.
7. **Direct hardware access remains a first-class citizen.** Inline assembly, interrupt functions, register-pinned locals, atomic LOCK regions, freestanding builds.
8. **A REPL that is the compiler** — top-level expressions execute, no `main()` is required, bare string literals print.
9. **Modern tooling baked in**: formatter, package manager, doc generator, LSP, debug info — all in a single `arsenal` binary.
10. **Cross-compilation is a first-class operation.** Like Zig: target triple is a flag.

### 5.3 Lexical Structure

- **Encoding**: UTF-8 source. String literals are `[]u8` (UTF-8). A `rune` type holds a Unicode scalar value (`u32`). 8-bit C-compat literals available as `c"..."`.
- **Comments**: `// line`, `/* block */`, and `///` doc comments (Markdown body, parsed by `arsenal doc`).
- **Identifiers**: `[A-Za-z_][A-Za-z0-9_]*`, ASCII-only at the lexer level (deliberate — keeps the lexer trivial, supports trivial editor tooling). Identifiers are case-sensitive and *not* underscore-collapsed (unlike Nim).
- **Numeric literals**: `123`, `0xFF`, `0o755`, `0b1010`, `1_000_000`, `3.14`, `1.0e9`, `0x1.fp10` (hex float). Underscores legal anywhere as digit separators.
- **Character literals**: `'A'` is a `rune` (UTF-8 scalar). `c'A'` is a `u8`. Multi-byte single-quoted (HolyC's `'ABC'`) is **rejected** by default (footgun), but available via `\#packed("ABC")` for explicit packed-integer construction.
- **String literals**: `"..."`, `\\multi\nline\\` raw strings (terminated by another `\\`), `c"..."` for null-terminated C strings.
- **Operators**: Standard set plus `..` (range exclusive), `..=` (range inclusive), `??` (nil-coalescing), `?.` (option chaining), `!!` (must-not-fail propagation, panics), `=>` (match arms), `<-` (channel send). **Removed**: `?:` ternary (replaced with `if expr1 then expr2 else expr3` — an expression form). HolyC's `` ` `` (power) operator is provided as `**`.
- **Keywords**: `fn`, `let`, `var`, `const`, `class`, `liberty`, `cipher`, `if`, `else`, `match`, `for`, `while`, `loop`, `break`, `continue`, `return`, `defer`, `errdefer`, `try`, `catch`, `foxdie`, `naked`, `pub`, `mod`, `use`, `as`, `in`, `where`, `comptime`, `inline`, `extern`, `rex`, `lock`, `fox`, `await`, `yield`, `true`, `false`, `nil`. Theme aliases (`#virtuous`, `liberty`, `cipher`, etc.) are reserved but lower-cased.
- **Statement terminator**: semicolons required between statements; trailing semicolons before `}` optional. Newlines are whitespace except inside `\\`-strings.

### 5.4 Type System

#### 5.4.1 Primitive Types

| Type | Size | Notes |
|---|---|---|
| `u0` | 0 | Zero-size unit (HolyC's `U0`). Function returning `u0` returns nothing. |
| `bool` | 1 | `true`/`false` |
| `i8`/`u8` … `i64`/`u64` | 1–8 B | Two's-complement signed/unsigned. |
| `i128`/`u128` | 16 B | Optional on 32-bit targets. |
| `usize`/`isize` | target ptr-width | |
| `iN`/`uN` for N in 1..256 | bit-packed | Like Zig, arbitrary-bit-width integers, useful in `packed class`. |
| `f32`, `f64` | IEEE-754 | Both supported (HolyC's F64-only stance is *not* preserved — F32 is needed for graphics/SIMD). |
| `f16`, `bf16` | IEEE / brain-float | Optional, gated on target. |
| `rune` | 4 | UTF-8 scalar value. |
| `nil` | — | Type of `nil`; assigns into any `?T`. |

#### 5.4.2 Composite Types

```gw
class Vec3 {
    x: f32,
    y: f32,
    z: f32,
}
```

- `class` introduces an aggregate POD type. **No methods**: methods are free functions with universal-call-syntax dispatch — `v.length()` desugars to `length(v)` if `length` is in scope. (Inspired by Nim, Odin, D's UFCS.)
- `class` may be `packed class { ... }` (bit-packed, no padding, ABI-defined), `extern class { ... }` (C-ABI layout guaranteed), or default (compiler-chosen layout).
- Inheritance is **deliberately absent**. Composition + traits.
- **Field metadata**, inherited from HolyC, is preserved: `class Player { hp: i32 @range(0, 100) @serialize, name: []u8 @display }`. The metadata is queryable via `@codec(T).fields[i].attrs`.

#### 5.4.3 Sum Types — `liberty`

```gw
liberty Event {
    KeyDown { key: rune, mods: u8 },
    KeyUp   { key: rune },
    Resize  { width: u32, height: u32 },
    Quit,
}

fn handle(e: Event) -> u0 {
    match e {
        Event.KeyDown{key, mods} => {
            "key %c with mods %X\n", key, mods;     // implicit Print()
        },
        Event.Quit => exit(0),
        else => {},
    }
}
```

- `liberty` declares a tagged union (sum type / ADT) with payload-bearing variants. Discriminant is implicit; `@codec` can introspect it.
- `match` is an expression and is exhaustiveness-checked.
- The keyword `liberty` is aliased to `enum union` for those who prefer plain English.

#### 5.4.4 Pointers, References, and Slices

- `*T` — raw pointer (nullable, no aliasing guarantees, allowed only in `naked` blocks or explicit `pub naked` functions).
- `&T` — borrowed reference, **non-null** by construction; lifetime tracked (see §5.5).
- `&mut T` — mutable borrow.
- `?&T`, `?T` — optional.
- `[]T` — *slice* (fat pointer + length); the universal sequence type.
- `[N]T` — fixed array (size known at compile time).
- `[*]T` — many-pointer (C-style pointer-to-array, used in FFI).
- `[*:0]u8` — sentinel-terminated pointer (C-style `char*`).
- `[dyn]T` — dynamic, allocator-aware growable array (analogous to Odin's `[dynamic]T`); carries an allocator handle in its header.

Nullability is **not** in pointers — references are non-null, and "maybe absent" is expressed via `?T`. This eliminates the entire null-pointer-dereference class of bugs in safe code.

#### 5.4.5 Generics — `ADAM`

GW uses **comptime parametric polymorphism** rather than C++/Rust-style monomorphisation-at-instantiation; the implementation strategy is monomorphisation but the *surface syntax* uses `comptime` parameters, à la Zig:

```gw
fn max[T: ADAM(Ord)](a: T, b: T) -> T {
    if a > b then a else b
}

class List[T: ADAM] {
    items: [dyn]T,
}
```

- `ADAM` is the root cipher (trait) of all types — equivalent to `any` / `type`. Any type satisfies `ADAM`.
- `ADAM(Cipher1, Cipher2)` is a constrained type parameter.
- Brackets `[...]` distinguish type parameters from value parameters `(...)`.
- All generic instantiation happens at compile time; there is no runtime type erasure (dyn-traits live in the cipher system; see below).

#### 5.4.6 Ciphers (Traits / Interfaces)

```gw
cipher Ord {
    fn cmp(self: &Self, other: &Self) -> i32;

    // default
    fn lt(self: &Self, other: &Self) -> bool {
        self.cmp(other) < 0
    }
}

cipher Display {
    fn fmt(self: &Self, w: &mut Writer) -> !u0;
}

class Vec3 satisfies Ord, Display {
    x: f32, y: f32, z: f32,

    fn cmp(self: &Self, other: &Self) -> i32 { ... }
    fn fmt(self: &Self, w: &mut Writer) -> !u0 { ... }
}
```

- Ciphers are structural by default but require explicit `satisfies` declaration (avoids accidental coupling — closer to Rust traits than Go interfaces).
- Dynamic dispatch via `dyn Cipher` (vtable + data pointer pair). Static dispatch via generics is the default.

#### 5.4.7 Type Inference

- `let x = 42;` infers `i32` (default integer width) — the compiler will emit a warning and require an annotation if the inferred width is inadequate (e.g., a literal `5_000_000_000` won't fit in `i32`).
- `let v: Vec3 = .{ .x = 1, .y = 2, .z = 3 };` — `.{ ... }` is the "anonymous-aggregate" expression that takes its type from context (Zig influence).
- Function return types are *not* inferred (HolyC and Zig agree here): explicit `-> T` is required. This is necessary for fast single-pass compilation and improves API stability.

### 5.5 Memory Model

GW's memory model is the most consequential design decision, balancing HolyC's complete openness against Rust's strict ownership and Zig's allocator discipline. The chosen synthesis:

#### 5.5.1 Three-Tier Safety

- **Safe (default)**: References (`&T`, `&mut T`) are checked by an Odin-grade "scope checker" — a simplified, non-Polonius lifetime analyser that enforces:
  - A reference may not outlive its referent.
  - At any point, either (a) one `&mut T` exists, or (b) any number of `&T` exist (no shared mutability).
  - Aliasing rules are checked function-locally (no whole-program analysis); references cannot escape the function unless tied to a parameter lifetime.
  - This is **deliberately less powerful than Rust** to keep compile times in the millisecond range. Programs requiring graphs and back-references are expected to use *handles into arenas* (the Odin / *zylinski* pattern), not borrows everywhere.

- **Manual (opt-in)**: `pub manual fn ...` opts out of borrow-checking but keeps null-checking and bounds-checking. Used where the borrow checker is too restrictive (e.g. doubly-linked lists).

- **Naked (fully unsafe)**: `naked { ... }` blocks unlock raw pointers (`*T`), arbitrary casts, pointer arithmetic, manual layout, and inline `rex` ASM. Required for kernel code, drivers, freestanding embedded.

#### 5.5.2 Allocators — the Arsenal Cipher

```gw
cipher Arsenal {
    fn alloc(self: &mut Self, size: usize, align: usize) -> !*naked u8;
    fn free(self: &mut Self, ptr: *naked u8, size: usize, align: usize) -> u0;
    fn resize(self: &mut Self, old: []u8, new_size: usize) -> ![]u8;
}
```

Every heap-allocating standard-library function takes either:

- an explicit `Arsenal`-implementing parameter (Zig style), **or**
- uses `context.allocator` if no override is given (Odin / Jai style).

This **dual approach** answers the longstanding ergonomic complaint about Zig (allocator plumbing in every signature) while preserving Zig's clarity advantage when desired:

```gw
// Implicit context allocator (Odin style)
let users = List[User].new();

// Explicit allocator (Zig style)
let users = List[User].new_in(arena);
```

The `context` is an Odin-borrowed implicit value carried on every GW-calling-convention call; it holds `allocator`, `temp_allocator`, `logger`, `rand`, `panic_handler`. It can be locally overridden:

```gw
{
    let arena = OuterHeaven.new(1 * MiB);
    defer arena.deinit();
    using_context(.{ .allocator = arena.allocator() }) {
        load_level("level1.json");   // every allocation flows into arena
    }   // arena freed in bulk on scope exit
}
```

#### 5.5.3 Built-in Allocators

`philosophers.alloc` provides:
- `Heap` — the system heap (libc malloc on hosted, a kernel slab on freestanding).
- `OuterHeaven` — bump arena, bulk-free on `deinit`.
- `OuterHeavenVirtual` — virtually-reserved, physically-committed growing arena (Odin's `vmem.Arena`).
- `Pool[T]` — fixed-size object pool.
- `Tracking` — wraps another allocator and reports leaks (debug builds).
- `Panic` — panics on any allocation (used to assert allocation-free regions).
- `FixedBuffer` — backed by a stack `[N]u8`.

#### 5.5.4 Resource Cleanup — `defer` / `errdefer`

GW deliberately does **not** have RAII destructors (a HolyC-aligned and Zig-aligned choice — RAII implies hidden control flow). Instead:

- `defer expr;` — runs `expr` when the enclosing scope exits, in reverse-declaration order.
- `errdefer expr;` — runs only if the scope exits via error propagation.

This produces clearer dataflow at the cost of more verbose cleanup code, and removes a major ABI/optimisation hazard.

### 5.6 Error Handling — `foxdie`

Error handling is a Zig-style error union. The keyword `foxdie` is the propagation operator (analogous to Zig's `try` and Rust's `?`); the rationale is the FOXDIE virus's role in MGS: a *targeted, identifier-tagged termination of a single thread of execution*, leaving the rest of the system untouched.

```gw
liberty FileError {
    NotFound,
    PermissionDenied,
    DiskFull,
    IO(io.Errno),
}

fn read_config(path: []u8) -> !FileError Config {
    let f  = foxdie fs.open(path);                // propagate FileError
    defer f.close();
    let bs = foxdie f.read_all(context.allocator);
    return foxdie Config.parse(bs);
}

fn main() -> u0 {
    let cfg = read_config("game.toml") catch |e| match e {
        FileError.NotFound => Config.default(),
        else => panic("config read failed: %", e),
    };
    "loaded %\n", cfg;
}
```

Key properties:

- `!E T` is the error-union type (error set `E`, success type `T`). If `E` is omitted, the compiler infers the error set from the function body (Zig-style error-set inference).
- `foxdie expr` is short-circuit propagation: if `expr` is an error, return it from the enclosing function.
- `expr catch |e| ...` recovers; the `|e|` payload binds the error.
- `expr catch fallback` is the simple non-binding form.
- **Error sets are open unions across modules** — `FileError | NetworkError` is meaningful.
- Errors carry a stack-of-error-call-sites trace in debug builds (Zig's "error return trace" idea).
- **No exceptions, no unwinding, no hidden control flow.**

### 5.7 Concurrency — Fox Tasks and Codec Channels

```gw
fn worker(jobs: codec_channel<Job>, results: codec_channel<Result>) -> u0 {
    for job in jobs {
        results <- compute(job);
    }
}

fn main() -> !u0 {
    let jobs    = codec_channel<Job>.bounded(64);
    let results = codec_channel<Result>.bounded(64);

    foreach _ in 0..8 {
        fox worker(jobs.clone(), results.clone());
    }
    // ... feed jobs, drain results
}
```

- **`fox` tasks** are M:N scheduled green threads (work-stealing scheduler), à la Go but cooperative. A `fox` runs until it hits an `await`, channel op, or explicit `yield`.
- **`codec_channel<T>`** — typed bounded MPMC channel; the natural communication primitive.
- **`async fn` / `await`** — for I/O-bound code, mapped onto an event loop.
- **`lock { ... }`** preserved from HolyC: applies platform LOCK prefixes / fence semantics for raw atomic regions in `naked` code.
- **`atomic[Order] T`** — typed atomic with explicit memory ordering.
- **Send/Sync ciphers** — `cipher Send` and `cipher Sync`, automatically derived (closed-world inference at compile time), are required to send a value across a `fox` boundary or share via channel. This gives data-race freedom in safe code.
- **Structured concurrency**: `nursery { ... }` blocks (Trio-style) wait for all child `fox`es before proceeding; cancellation propagates downward.

### 5.8 Compilation Model

#### 5.8.1 Single-Pass with Backpatching

GW is compiled by a deliberately simple, single-pass front-end emitting a small, register-flavoured IR that is consumed by either a **fast non-optimising backend** (TPDE-style template-based codegen, used by default and by the JIT) or by **LLVM** for optimised release builds. The fast backend's compile speed target is ≥ 1 M LoC/s/core; LLVM is opt-in (`arsenal build --release`).

The single-pass discipline is preserved by a backpatching strategy:

- Forward references are recorded in a fixup table.
- At end-of-translation-unit, fixups are resolved.
- This is exactly HolyC's strategy, modernised.

#### 5.8.2 Module System — Mother Base

- A **frequency** (module/package) is a directory containing `frequency.gw` (manifest) and `.gw` source files. Filenames have no semantic meaning; the compiler crawls the directory.
- `mod foo;` and `use foo.{Bar, baz};` introduce names. There are **no headers, no forward declarations, no preprocessor.** Function/type/const ordering within a frequency is irrelevant.
- A `MotherBase.gw` at the project root drives the `arsenal build` command and is itself executable GW code (Jai-style: the build system is the language).

#### 5.8.3 Cross-Compilation

`arsenal build --target x86_64-linux-gnu` and similar flags. Like Zig, the compiler bundles a libc cross-compilation matrix (musl, glibc, Windows, macOS, freestanding) so cross-builds are first-class. WebAssembly, AArch64, RISC-V, x86-64 are supported targets at v1.

#### 5.8.4 Comptime — `#virtuous`

Any expression or block prefixed `#virtuous` is evaluated at compile time using the same evaluator as the runtime (a stack VM in the front-end). Comptime can:

- Compute constants.
- Generate types — `fn Pair(comptime A: type, comptime B: type) -> type { return class { a: A, b: B } }`.
- Inspect `@codec(T)` and emit code based on it.
- **`#insert(s: []u8)`** (Jai-borrowed) inserts a string of GW source at the call site.
- **`#run expr`** is a comptime call, returning a value baked into the binary.

This single mechanism subsumes generics, conditional compilation, build scripts, AST macros, and serialisation codegen.

#### 5.8.5 JIT-as-Shell Mode

`arsenal repl` (or simply `codec`) launches a single-task GW environment in which the user types statements and they execute. The same compiler is used; there is no separate interpreter. Top-level declarations install into a persistent symbol table for the session; bare expressions print their result; bare strings invoke `Print`. This preserves HolyC's most distinctive ergonomic feature.

```
$ codec
GW codec 1.0   (philosophers stdlib loaded)
> 5 + 7;
12
> "hello %\n", "world";
hello world
> fn fib(n: i64) -> i64 { if n < 2 then n else fib(n-1) + fib(n-2) };
> fib(20);
6765
> use std.fs;
> fs.read_to_string("readme.md") catch "(missing)";
"# GW ..."
```

### 5.9 Standard Library — Philosophers

The `philosophers` library is small, allocator-aware, and avoids hidden allocations. Top-level packages:

- `philosophers.mem` — allocators, `OuterHeaven`, slices, `mem.copy`, `mem.set`.
- `philosophers.io` — `Reader`, `Writer` ciphers; buffered I/O; UTF-8-safe text.
- `philosophers.fs` — files, paths, async file ops.
- `philosophers.os` — process, env, signals.
- `philosophers.net` — sockets, TLS (built-in via BoringSSL bind), HTTP/1+2 client/server.
- `philosophers.fox` — tasks, channels, mutex, RWLock, `nursery`.
- `philosophers.fmt` — `fmt.print`, `fmt.println`, `fmt.bprint` (Odin-style).
- `philosophers.codec` — runtime reflection (the `@codec(T)` companion API).
- `philosophers.json`, `philosophers.toml`, `philosophers.bin` — serialisation, all auto-derived from class metadata.
- `philosophers.math`, `philosophers.simd`, `philosophers.gfx` (vectors, matrices, quaternions à la Odin — first-class).
- `philosophers.test` — the `snake_eater { }` test runner.
- `philosophers.collections` — `Map`, `Set`, `Ring`, intrusive linked-list, handle-map.

There is **no `String` class**; `[]u8` is the universal text type with helpers in `philosophers.text` (UTF-8-aware).

### 5.10 Tooling — All in `arsenal`

| Subcommand | Purpose |
|---|---|
| `arsenal build` | Build the current Mother Base. |
| `arsenal run` | Build + run. |
| `arsenal test` | Run all `snake_eater { }` blocks. |
| `arsenal bench` | Run all `virtuous_mission { }` blocks. |
| `arsenal fmt` | Canonical formatter (no options — Go-style). |
| `arsenal doc` | Generate Markdown/HTML docs from `///` comments + class metadata. |
| `arsenal lsp` | Language server (built-in). |
| `arsenal codec` | REPL / live shell. |
| `arsenal cipher` | Show/install/update package dependencies. |
| `arsenal disasm` | Disassemble emitted code (the descendant of HolyC's `Uf()`). |

### 5.11 Interop — Cipher

```gw
cipher "C" extern fn malloc(size: usize) -> *naked u8;
cipher "C" extern fn free(ptr: *naked u8) -> u0;

cipher "C" pub fn gw_callback(arg: *naked u8) -> i32 {
    // exposes a C-ABI symbol named gw_callback
}
```

- `cipher "C"` declares the C ABI; other ABIs (`"win64"`, `"sysv"`, `"interrupt"`, `"naked"`) are supported.
- `arsenal build --import-c foo.h` runs Clang to lower a C header to GW declarations directly (Zig's `@cImport` model). No bindings file needed.
- C structs are `extern class { ... }` with C-compatible layout.

### 5.12 Inline Assembly — `rex` Blocks

```gw
fn rdtsc() -> u64 {
    let lo: u32 = 0;
    let hi: u32 = 0;
    rex {
        RDTSC
        MOV [&lo], EAX
        MOV [&hi], EDX
    }
    return (hi as u64) << 32 | (lo as u64);
}

#[interrupt]
fn irq_keyboard() -> u0 naked {
    rex {
        IN  AL, 0x60
        ...
        IRET
    }
}
```

- `rex` blocks accept the platform-canonical mnemonics (Intel syntax on x86, ARM syntax on AArch64).
- GW locals are visible by name (HolyC pattern); register pinning via `let r15 reg(R15) i: i64 = 5` for tight integration.
- `#[interrupt]` and `#[naked]` function attributes mirror HolyC's `interrupt` / `argpop` flags.
- `lock { ... }` block-prefixes apply LOCK semantics across all atomic ops within (HolyC pattern).

### 5.13 Reflection — `@codec`

```gw
class Player { hp: i32 @range(0, 100), name: []u8 @display }

fn dump[T: ADAM](v: &T) -> u0 {
    const info = @codec(T);
    "%(", info.name;
    inline for f in info.fields {
        "%=%, ", f.name, @field(v, f.name);
        // attribute-driven behaviour:
        if f.has_attr("display") then highlight();
    }
    ")";
}
```

- `@codec(T)` returns a comptime-known `TypeInfo` value with fields, sizes, alignments, attributes, and (for `liberty`) variant info.
- `@field(v, "name")` and `@call(fn, args)` close over field-name and function-name strings at comptime.
- `inline for` unrolls a comptime-known loop over `info.fields` — the standard pattern for serialisation, ORM, command parsing, and similar.
- This subsumes HolyC's `ClassRep()` / `DocForm()` / `lastclass` features and replaces them with a single, principled API.

### 5.14 DolDoc, Modernised — Doc Sources

GW preserves the *spirit* of DolDoc (rich content in source) without inventing a new file format. The compiler accepts two kinds of source files:

- `*.gw` — pure-text GW source, the universal interchange format. **No tool needs anything more than a UTF-8 editor to read GW.** This is the deliberate departure from DolDoc's biggest failure.
- `*.gw.md` — *literate* GW: a Markdown file containing fenced ```gw code blocks. The compiler tangles the code blocks; everything else is documentation. This integrates with the existing Markdown ecosystem (renderers, GitHub, LSPs).

For *embedded media in source*, GW provides `#asset("path/to/sprite.png")` — a comptime intrinsic that bakes the file's bytes into the binary as a `[]const u8`. Together with `arsenal doc`, this lets you embed images and diagrams in documentation without coupling the source format to a custom binary representation.

### 5.15 Sample Programs

#### 5.15.1 Hello, World — minimal, HolyC-style

```gw
"Behold the Outer Heaven.\n";
```

That's the entire program. No `main`, no imports, top-level statement. The bare string literal invokes `Print`.

#### 5.15.2 Structs, Generics, Methods (UFCS)

```gw
class Vec3 { x: f32, y: f32, z: f32 }

fn dot(a: &Vec3, b: &Vec3) -> f32 {
    a.x * b.x + a.y * b.y + a.z * b.z
}

fn length(v: &Vec3) -> f32 {
    @import("philosophers.math").sqrt(dot(v, v))
}

let v = Vec3{ .x = 1.0, .y = 2.0, .z = 2.0 };
"length = %\n", v.length();   // UFCS: v.length() == length(&v)
```

#### 5.15.3 Sum Types and Error Handling

```gw
liberty Tile { Floor, Wall, Door{ locked: bool } }

liberty MapError { OutOfBounds, MalformedFile }

fn tile_at(map: &Map, x: i32, y: i32) -> !MapError Tile {
    if x < 0 or y < 0 or x >= map.w or y >= map.h
        then return MapError.OutOfBounds;
    return map.cells[y * map.w + x];
}

fn describe(map: &Map, x: i32, y: i32) -> u0 {
    let t = tile_at(map, x, y) catch |e| match e {
        MapError.OutOfBounds => { "off-map\n"; return; },
        else => panic("map error: %", e),
    };
    match t {
        Tile.Floor          => "open floor\n",
        Tile.Wall           => "stone wall\n",
        Tile.Door{locked}   => "% door\n", if locked then "locked" else "open",
    }
}
```

#### 5.15.4 Generics with Cipher Constraint

```gw
cipher Hash {
    fn hash(self: &Self) -> u64;
    fn equals(self: &Self, other: &Self) -> bool;
}

class HashMap[K: ADAM(Hash), V: ADAM] {
    buckets: [dyn]?Pair[K, V],
    arsenal: Arsenal,
}

fn HashMap[K, V].insert(self: &mut Self, k: K, v: V) -> !u0 {
    let h = k.hash();
    // ... open-addressing logic
}
```

#### 5.15.5 Memory Management with Arena

```gw
fn load_level(path: []u8) -> !LoadError Level {
    let arena = OuterHeavenVirtual.new();
    errdefer arena.deinit();   // free on error path only

    using_context(.{ .allocator = arena.allocator() }) {
        let data  = foxdie fs.read_all(path);
        let level = foxdie json.parse[Level](data);
        // every allocation inside parse() flowed into the arena
        return Level{ .data = level, .arena = arena };
    }
}

fn unload_level(l: &mut Level) -> u0 {
    l.arena.deinit();   // bulk free — no per-field bookkeeping
}
```

This is the *Odin/Karl-Zylinski pattern* lifted directly: arenas express common-lifetime grouping without RAII or borrow checking.

#### 5.15.6 Concurrency

```gw
fn pipeline(urls: []const []u8) -> !u0 {
    let work    = codec_channel[[]u8].bounded(32);
    let pages   = codec_channel[Page].bounded(32);

    nursery {
        // producer
        fox {
            for u in urls { work <- u; }
            work.close();
        };

        // 8 fetchers
        for _ in 0..8 {
            fox {
                for u in work {
                    let page = http.get(u) catch continue;
                    pages <- page;
                }
            };
        }

        // single consumer
        fox {
            for p in pages { index(p); }
        };
    }   // nursery waits for all fox tasks here
}
```

#### 5.15.7 FFI with C

```gw
cipher "C" extern fn SDL_Init(flags: u32) -> i32;
cipher "C" extern fn SDL_Quit() -> u0;

const SDL_INIT_VIDEO: u32 = 0x20;

fn main() -> !u0 {
    if SDL_Init(SDL_INIT_VIDEO) != 0 {
        return foxdie InitError.SdlFailed;
    }
    defer SDL_Quit();
    // ...
}
```

#### 5.15.8 Compile-Time Codegen with `#virtuous`

```gw
fn make_serialiser[T: ADAM]() -> fn(&T, &mut Writer) -> !u0 {
    return #virtuous {
        const info = @codec(T);
        // Build an unrolled writer for T's fields.
        let buf: [dyn]u8 = .{};
        buf.push("fn(v: &T, w: &mut Writer) -> !u0 { foxdie w.put(\"{\");");
        inline for f, i in info.fields {
            if i > 0 { buf.push("foxdie w.put(\",\");"); }
            buf.push_fmt("foxdie w.put(\"%=\");", f.name);
            buf.push_fmt("foxdie write(&v.%, w);", f.name);
        }
        buf.push("foxdie w.put(\"}\"); return; }");
        #insert(buf.to_slice())
    };
}
```

This single comptime mechanism handles serialisers, parsers, command tables, jump tables, and SoA struct generation — the work that templates and macros do in C++/Rust.

### 5.16 Justification of Design Choices

The table summarises the lineage of each GW feature:

| GW feature | Origin | Rationale |
|---|---|---|
| Single-pass + JIT-as-shell | **HolyC** | The single best feature of HolyC, lost in every other systems language since Forth. |
| No headers, no preprocessor | HolyC, Zig, Odin, Jai | Compile-speed win + complexity reduction. |
| No required `main`, top-level execution | **HolyC** | REPL-equivalent ergonomics for free. |
| Implicit `Print` of bare strings | **HolyC** | Tiny but distinctive shell-friendliness. |
| Default args anywhere with named holes | **HolyC** | Genuinely useful, no significant cost. |
| `u0` zero-size unit | **HolyC** | Correct, in contrast to C's `sizeof(void)==1`. |
| `i8..i64`, `u8..u64`, arbitrary-bit-width ints | HolyC + Zig | HolyC's typing scheme generalised. |
| Class metadata + `@codec` reflection | **HolyC** + Zig | Modernises HolyC's per-field metadata story. |
| `lastclass`-style intro is unnecessary | (subsumed) | Rolled into general comptime reflection. |
| `comptime` / `#virtuous` | **Zig + Jai** | Single mechanism for all metaprogramming. |
| `#insert` and `#run` | **Jai** | Compile-time codegen. |
| `liberty` sum types | Rust + Zig + ML | First-class ADTs are non-negotiable. |
| `?T` optional, non-null `&T` | Rust + Zig | Eliminates null-deref class. |
| `!T` error union with `foxdie` | **Zig** | Cleanest known error model. |
| Allocator-as-parameter + implicit `context` | **Zig + Odin + Jai** | Best-of-both: explicit when you want, ergonomic when you don't. |
| Arenas (`OuterHeaven`) as a primary discipline | **Odin** | Practical alternative to borrow checking for non-tree data. |
| `defer` / `errdefer` (no RAII) | **Zig** | Avoids hidden control flow. |
| Lifetime-checked `&T`, opt-out `manual`, opt-out `naked` | **Rust** softened | Safety as a gradient, not a binary. |
| `fox` / `nursery` structured concurrency | Trio + Go + Rust | Best-of-modern. |
| `lock { }`, `rex { }`, register pinning, `interrupt` | **HolyC** | Ring-0 / kernel use cases preserved. |
| Built-in cross-compilation | **Zig** | Modern table stakes. |
| Built-in formatter, LSP, doc gen | Go + Rust + Zig | Tooling included by default. |
| Markdown literate sources (`*.gw.md`) | **HolyC's DolDoc spirit**, modernised | Rich docs in source without a custom binary format. |
| `#asset()` for embedded binary data | **HolyC sprite tags** | Same affordance, portable representation. |
| C ABI as default `cipher` | C + Zig | Interop is the path of adoption. |
| Theme-keyword aliasing (`liberty` = `enum union`) | Original | Naming gives character; aliases keep technical readers comfortable. |

### 5.17 What GW Deliberately Omits

To preserve the speed-first, low-complexity goals, GW *rejects* several mainstream features:

- **No exceptions, no stack unwinding.** `foxdie`/`catch` only.
- **No RAII destructors.** `defer`/`errdefer` only.
- **No operator overloading on user types**, except for a small whitelist (`Add`, `Sub`, `Mul`, `Div`, `Eq`, `Ord`, `Hash`, `Display`) tied to ciphers — same approach as Rust traits, no surprise calls.
- **No implicit conversions** between numeric types (Zig rule).
- **No inheritance.** Composition + ciphers.
- **No GC, no ARC.** Allocators + arenas + lifetime checks.
- **No closures that capture by reference across `fox` boundaries** without `Send`-derivation.
- **No global mutable state without `unsafe`/`naked`.**
- **No procedural macros** (everything is `#virtuous`).
- **No reflection at runtime by default** — `@codec(T)` is comptime; runtime type info costs an explicit `dyn` annotation.

### 5.18 Conclusion

HolyC is, to a startling degree, a coherent piece of language design: a language whose every choice was made to support a single-developer, single-machine, single-image, ring-0 environment in which the compiler is the shell. Strip away the religious cosmology and what remains is a strikingly modern thesis — *fast compilation, no headers, top-level execution, compile-time text generation, embedded rich content, ergonomic inline assembly, and the abolition of the edit-compile-run cycle* — that anticipated by a decade the design centres of Zig, Odin, and Jai.

GW is the proposal to take those ideas seriously and combine them with the modern systems-language toolkit: a checked but pragmatic memory model, sum types, error unions, comptime metaprogramming, async tasks, cross-compilation, and an everything-in-one toolchain. The result is a language that should be possible to use as both a kernel-implementation tool *and* an interactive shell — the original promise of HolyC, finally made portable, safe enough for production, and equipped for 2020s hardware.

The codename — drawn from the AI that mediated information across the *Sons of Liberty* network — is fitting: a compiler is exactly that. An optic-neural arbiter between the programmer's tactical intent and the machine's execution. Whether the user is building a kernel, a game engine, or a one-off shell pipeline, GW means to be the channel.

> **`> arsenal codec`**
> **`GW 1.0 — kept you waiting, huh?`**
> **`> "Mission complete.\n";`**
> **`Mission complete.`**