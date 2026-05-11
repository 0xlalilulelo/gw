# GW — A Modern Systems Programming Language

*A modern systems language synthesizing Zig's comptime and cross-compilation, Odin's allocator pragmatism, Rust's safety story, and Jai-class compile speed, with a JIT REPL that none of them currently provide.*

---

## Part 1 — Design Goals and Philosophy

GW is a systems programming language for kernel hackers, game engine engineers, embedded firmware teams, and performance-critical infrastructure. It targets the niche currently fragmented across Rust (safety, ecosystem), Zig (simplicity, comptime), Odin (pragmatism), and Jai (compile speed). GW's thesis is that these properties are not mutually exclusive — a language can offer all four with deliberate design.

The design is governed by seven goals, in priority order:

1. **Fast compilation.** Target throughput of 1M LoC/s/core for the development backend, comparable to Jai's reported numbers and ~4× LLVM `-O0` as a hard fallback. This is implemented via a TPDE-style template-driven non-optimizing backend; the user-facing property is responsiveness, not the algorithm.
2. **Memory safety as a gradient.** Three explicit tiers — `safe` (region-checked references, default), `manual` (opt-out borrow check), `unsafe` (full raw access) — recognizing that safe-by-default and kernel-by-necessity are both legitimate program states.
3. **One mechanism for metaprogramming.** Zig/Jai-style `comptime` execution subsumes generics, conditional compilation, build scripts, and code generation.
4. **Errors as values, not exceptions.** Tagged unions and error sets are first-class; no stack unwinding.
5. **C ABI as the lingua franca.** GW links to and from C with no shim layer.
6. **Direct hardware access remains a first-class citizen.** Inline assembly, interrupt functions, register-pinned locals, atomic regions, freestanding builds.
7. **Modern tooling baked in.** Formatter, package manager, doc generator, LSP, debug info, JIT REPL — all in a single `gw` binary.

A JIT REPL (`gw repl`) with a persistent symbol table is included from day one. This places GW in the company of Julia, F# Interactive, and Swift Playgrounds for interactive workflow, but with native AOT performance and systems-language semantics. None of GW's primary competitors — Rust, Zig, Odin, Jai — provide a comparable interactive surface.

---

## Part 2 — Naming and Style Conventions

GW deliberately uses plain, predictable names for every language feature and every tool. A reader should be able to guess what `trait`, `enum`, `task`, `channel`, `comptime`, and `unsafe` mean without consulting a glossary. This is a design choice, not an aesthetic one: predictable names reduce the time-to-fluency for human readers and substantially improve the rate at which language models generate correct GW code on the first attempt.

The conventions:

- **Source files**: `snake_case.gw`. Modules are directories; the directory name is the module name.
- **Markdown literate sources**: `topic_name.gw.md`. Fenced ```gw blocks are extracted and compiled in source order.
- **Type names** (classes, enums, traits): `PascalCase` — `Allocator`, `Reader`, `HashMap`, `KeyEvent`.
- **Function names, variables, fields**: `snake_case` — `read_to_string`, `len`, `parse_json`.
- **Constants**: `SCREAMING_SNAKE_CASE` — `MAX_PATH_LEN`, `DEFAULT_BUFFER_SIZE`.
- **Compiler attributes**: `#[snake_case]` — `#[interrupt]`, `#[naked]`, `#[no_alloc]`, `#[must_use]`.
- **Comptime intrinsics**: `@snake_case` — `@type_info`, `@field`, `@call`, `@embed_file`.
- **Project root manifest**: `build.gw` (an executable GW file; the build system is the language).

The driver binary is `gw`. Every subcommand (`gw build`, `gw run`, `gw test`, `gw repl`, `gw fmt`, `gw doc`, `gw lsp`, `gw pkg`) follows the same shape. Subcommand names are imperative verbs or short nouns; no abbreviations beyond the universally recognized (`fmt`, `pkg`, `repl`).

---

## Part 3 — Lexical Structure

- **Encoding**: UTF-8 source. String literals are `[]u8` (UTF-8). A `rune` type holds a Unicode scalar value (`u32`). 8-bit C-compat literals available as `c"..."`.
- **Comments**: `// line`, `/* block */`, and `///` doc comments (Markdown body, parsed by `gw doc`).
- **Identifiers**: `[A-Za-z_][A-Za-z0-9_]*`, ASCII at the lexer level (keeps lexer trivial, supports trivial editor tooling). Case-sensitive, not underscore-collapsed.
- **Numeric literals**: `123`, `0xFF`, `0o755`, `0b1010`, `1_000_000`, `3.14`, `1.0e9`, `0x1.fp10` (hex float). Underscores legal anywhere as digit separators.
- **Character literals**: `'A'` is a `rune`. `c'A'` is a `u8`. Multi-byte single-quoted literals are rejected; explicit packed-integer construction uses `\#packed("ABC")`.
- **String literals**: `"..."`, `\\multi\nline\\` raw strings, `c"..."` for null-terminated C strings.
- **Operators**: Standard set plus `..` (range exclusive), `..=` (range inclusive), `??` (nil-coalescing), `?.` (option chaining), `!!` (must-not-fail propagation, panics), `=>` (match arms), `<-` (channel send). Power operator is `**`. Removed: `?:` ternary (replaced with `if cond then a else b` or block-form `if cond { a } else { b }`).
- **Keywords**: `fn`, `let`, `var`, `const`, `class`, `enum`, `trait`, `impl`, `if`, `else`, `match`, `for`, `while`, `loop`, `break`, `continue`, `return`, `defer`, `errdefer`, `try`, `catch`, `unsafe`, `pub`, `mod`, `use`, `as`, `in`, `where`, `comptime`, `inline`, `extern`, `asm`, `lock`, `task`, `async`, `await`, `yield`, `true`, `false`, `nil`, `then`.
- **Statement terminator**: Semicolons required between statements; trailing semicolons before `}` optional. Newlines are whitespace except inside `\\`-strings.

---

## Part 4 — Type System

### 4.1 Primitive Types

| Type | Size | Notes |
|---|---|---|
| `u0` | 0 | Zero-size unit. Function returning `u0` returns nothing. (Rust `()`, Zig `void`.) |
| `bool` | 1 | `true` / `false`. |
| `i8`/`u8` … `i64`/`u64` | 1–8 B | Two's-complement signed/unsigned. |
| `i128`/`u128` | 16 B | Optional on 32-bit targets. |
| `usize`/`isize` | target ptr-width | |
| `iN`/`uN` for N in 1..256 | bit-packed | Arbitrary-bit-width integers, useful in `packed class`. |
| `f32`, `f64` | IEEE-754 | Both supported. |
| `f16`, `bf16` | IEEE / brain-float | Optional, gated on target. |
| `rune` | 4 | UTF-8 scalar value. |
| `nil` | — | Type of `nil`; assigns into any `?T`. |

### 4.2 Composite Types

```gw
class Vec3 {
    x: f32,
    y: f32,
    z: f32,
}
```

`class` introduces an aggregate POD type. **No methods inside the class body** — methods are free functions, or are placed in `impl` blocks (see 4.6). Free functions can be invoked with universal-call-syntax dispatch (UFCS): `v.length()` desugars to `length(v)` if `length` is in scope. Inspired by Nim, Odin, and D's UFCS.

Variants:
- `packed class { ... }` — bit-packed, no padding, ABI-defined
- `extern class { ... }` — C-ABI layout guaranteed
- Default — compiler-chosen layout

**Inheritance is deliberately absent.** Composition + traits.

**Field metadata** is preserved through compilation, enabling reflection and serde codegen:
```gw
class Player {
    hp: i32 @range(0, 100) @serialize,
    name: []u8 @display
}
```
Metadata is queryable via `@type_info(T).fields[i].attrs`. This pattern is shared with Rust attribute macros, C# attributes, and Java annotations.

### 4.3 Sum Types — `enum`

```gw
enum Event {
    KeyDown { key: rune, mods: u8 },
    KeyUp   { key: rune },
    Resize  { width: u32, height: u32 },
    Quit,
}

fn handle(e: Event) -> u0 {
    match e {
        Event.KeyDown{key, mods} => {
            print("key %c with mods %X\n", key, mods);
        },
        Event.Quit => exit(0),
        else => {},
    }
}
```

`enum` declares a tagged union with payload-bearing variants. Discriminant is implicit; `@type_info` can introspect it. `match` is an expression and is exhaustiveness-checked. GW's `enum` is always tagged (Rust-style); there is no C-style untagged enum.

### 4.4 Pointers, References, and Slices

- `*T` — raw pointer (nullable, no aliasing guarantees, allowed only in `unsafe` blocks or `pub unsafe` functions).
- `&T` — borrowed reference, **non-null** by construction; lifetime tracked.
- `&mut T` — mutable borrow.
- `?&T`, `?T` — optional.
- `[]T` — slice (fat pointer + length); the universal sequence type.
- `[N]T` — fixed array (size known at compile time).
- `[*]T` — many-pointer (C-style pointer-to-array, used in FFI).
- `[*:0]u8` — sentinel-terminated pointer (C-style `char*`).
- `[dyn]T` — dynamic, allocator-aware growable array (analogous to Odin's `[dynamic]T`).

Nullability is **not** in pointers — references are non-null; "maybe absent" is `?T`. This eliminates the entire null-pointer-dereference class of bugs in safe code.

### 4.5 Generics

GW uses **comptime parametric polymorphism** with monomorphization:

```gw
fn max[T: any + Ord](a: T, b: T) -> T {
    if a > b then a else b
}

class List[T: any] {
    items: [dyn]T,
}
```

- `any` is the unconstrained type parameter (every type satisfies `any`). It is the root trait — equivalent to Rust's no-bound or Zig's `comptime T: type`.
- `T: Trait1 + Trait2` is a constrained type parameter.
- Brackets `[...]` distinguish type parameters from value parameters `(...)`.
- All generic instantiation happens at compile time via monomorphization. There is no runtime type erasure (dynamic dispatch lives in `dyn Trait`).

### 4.6 Traits

```gw
trait Ord {
    fn cmp(self: &Self, other: &Self) -> i32;

    // default method
    fn lt(self: &Self, other: &Self) -> bool {
        self.cmp(other) < 0
    }
}

class Vec3 {
    x: f32, y: f32, z: f32,
}

impl Ord for Vec3 {
    fn cmp(self: &Self, other: &Self) -> i32 { ... }
}

impl Display for Vec3 {
    fn fmt(self: &Self, w: &mut Writer) -> !u0 { ... }
}
```

Traits are explicit-impl by default — every `impl Trait for Type` registers a single contract. This avoids accidental coupling (closer to Rust traits than Go interfaces). Dynamic dispatch via `dyn Trait` (vtable + data pointer pair); static dispatch via generics is the default.

### 4.7 Type Inference

Bidirectional inference (Dunfield & Krishnaswami, 2013):

- `let x = 42;` infers `i32` (default integer width).
- `let v: Vec3 = .{ .x = 1, .y = 2, .z = 3 };` — `.{ ... }` is the anonymous-aggregate expression that takes its type from context (Zig influence).
- Function return types are not inferred; explicit `-> T` is required. Necessary for fast single-pass compilation, improves API stability.

---

## Part 5 — Memory Model

### 5.1 Three-Tier Safety

GW's memory model balances Rust's strict ownership against Zig's allocator discipline.

- **Safe (default)**: References (`&T`, `&mut T`) are checked by an Odin-grade scope checker — a simplified, non-Polonius lifetime analyser:
  - A reference may not outlive its referent.
  - At any program point, either one `&mut T` exists, or any number of `&T` (no shared mutability).
  - Aliasing rules checked function-locally (no whole-program analysis).
  - References cannot escape the function unless tied to a parameter lifetime.
  - **Less powerful than Rust** to keep compile times in the millisecond range. Programs requiring graphs and back-references use *handles into arenas* (the Odin pattern), not borrows everywhere.

- **Manual (opt-in)**: `pub manual fn ...` opts out of borrow-checking but retains null-checking and bounds-checking. For doubly-linked lists and similar.

- **Unsafe (fully unsafe)**: `unsafe { ... }` blocks unlock raw pointers, arbitrary casts, pointer arithmetic, manual layout, and inline `asm`. Required for kernel code, drivers, freestanding embedded.

### 5.2 Allocators — the Allocator Trait

```gw
trait Allocator {
    fn alloc(self: &mut Self, size: usize, align: usize) -> ![*]u8;
    fn free(self: &mut Self, ptr: [*]u8, size: usize, align: usize) -> u0;
    fn resize(self: &mut Self, old: []u8, new_size: usize) -> ![]u8;
}
```

Every heap-allocating standard-library function takes either:
- An explicit `Allocator`-implementing parameter (Zig style), **or**
- Uses `context.allocator` if no override given (Odin / Jai style).

```gw
// Implicit context allocator (Odin style)
let users = List[User].new();

// Explicit allocator (Zig style)
let users = List[User].new_in(arena);
```

The `context` is borrowed from Odin: an implicit value carried on every GW-calling-convention call holding `allocator`, `temp_allocator`, `logger`, `rand`, `panic_handler`. Locally overridable:

```gw
{
    let arena = Arena.new(1 * MiB);
    defer arena.deinit();
    using_context(.{ .allocator = arena.allocator() }) {
        load_level("level1.json");   // every allocation flows into arena
    }   // arena freed in bulk on scope exit
}
```

### 5.3 Built-in Allocators

`std.mem` provides:
- **Heap** — system heap (libc malloc on hosted, kernel slab on freestanding).
- **Arena** — bump arena, bulk-free on `deinit`.
- **VirtualArena** — virtually-reserved, physically-committed growing arena (Odin's `vmem.Arena`).
- **Pool[T]** — fixed-size object pool.
- **Tracking** — wraps another allocator and reports leaks (debug builds).
- **Panic** — panics on any allocation (asserts allocation-free regions).
- **FixedBuffer** — backed by a stack `[N]u8`.

### 5.4 Resource Cleanup — `defer` / `errdefer`

GW deliberately does **not** have RAII destructors (Zig-aligned choice — RAII implies hidden control flow). Instead:

- `defer expr;` — runs `expr` when the enclosing scope exits, in reverse-declaration order.
- `errdefer expr;` — runs only if the scope exits via error propagation.

Produces clearer dataflow at the cost of more verbose cleanup code, removing a major ABI/optimisation hazard.

---

## Part 6 — Error Handling

Error handling is a Zig-style error union. The keyword `try` is the propagation operator: short-circuit return on the error variant, unwrap on the success variant.

```gw
enum FileError {
    NotFound,
    PermissionDenied,
    DiskFull,
    IO(io.Errno),
}

fn read_config(path: []u8) -> !FileError Config {
    let f  = try fs.open(path);                   // propagate FileError
    defer f.close();
    let bs = try f.read_all(context.allocator);
    return try Config.parse(bs);
}

fn main() -> u0 {
    let cfg = read_config("game.toml") catch |e| match e {
        FileError.NotFound => Config.default(),
        else => panic("config read failed: %", e),
    };
    print("loaded %\n", cfg);
}
```

Properties:
- `!E T` is the error-union type. If `E` is omitted, the compiler infers the error set from the function body (Zig-style).
- `try expr` is short-circuit propagation.
- `expr catch |e| ...` recovers; the `|e|` payload binds the error.
- `expr catch fallback` is the simple non-binding form.
- **Error sets are open unions across modules** — `FileError | NetworkError` is meaningful.
- Error return traces in debug builds (Zig pattern).
- **No exceptions, no unwinding, no hidden control flow.**

---

## Part 7 — Concurrency

```gw
fn worker(jobs: channel<Job>, results: channel<Result>) -> u0 {
    for job in jobs {
        results <- compute(job);
    }
}

fn main() -> !u0 {
    let jobs    = channel<Job>.bounded(64);
    let results = channel<Result>.bounded(64);

    for _ in 0..8 {
        task worker(jobs.clone(), results.clone());
    }
    // ... feed jobs, drain results
}
```

- **`task`** spawns an M:N scheduled green thread (work-stealing scheduler).
- **`channel<T>`** — typed bounded MPMC channel.
- **`async fn` / `await`** — for I/O-bound code, mapped onto an event loop.
- **`lock { ... }`** applies platform LOCK prefixes / fence semantics for raw atomic regions in `unsafe` code (standard concurrency primitive — Java `synchronized`, C# `lock`).
- **`atomic[Order] T`** — typed atomic with explicit memory ordering.
- **Send/Sync traits** — auto-derived (closed-world inference at compile time); required to send a value across a `task` boundary or share via channel. Data-race freedom in safe code.
- **Structured concurrency**: `nursery { ... }` blocks (Trio-style) wait for all child tasks before proceeding; cancellation propagates downward.

---

## Part 8 — Compilation Model

### 8.1 Fast Compilation as Implementation Strategy

GW targets 1M LoC/s/core development-build throughput. The implementation uses a single-pass front-end with backpatching for forward references, feeding a TPDE-style template-driven non-optimizing backend. The single-pass discipline is an implementation choice; the user-facing property is fast, responsive builds.

LLVM is invoked only for `--release` builds where output quality matters more than compile latency.

### 8.2 Module System

- A **module** (package) is a directory containing a `mod.gw` (manifest) and `.gw` source files.
- `mod foo;` and `use foo.{Bar, baz};` introduce names.
- **No headers, no forward declarations, no preprocessor.** Function/type/const ordering within a module is irrelevant.
- A `build.gw` at the project root drives `gw build` and is itself executable GW code (Jai-style: the build system is the language).

### 8.3 Cross-Compilation

`gw build --target x86_64-linux-gnu`. Like Zig, the compiler bundles a libc cross-compilation matrix (musl, glibc, Windows, macOS, freestanding) so cross-builds are first-class. WebAssembly, AArch64, RISC-V, x86-64 are supported targets at v1.

### 8.4 Comptime

Any expression or block prefixed `comptime` is evaluated at compile time using the same evaluator as runtime (a stack VM in the front-end). Comptime can:

- Compute constants.
- Generate types — `fn Pair(comptime A: type, comptime B: type) -> type { return class { a: A, b: B } }`.
- Inspect `@type_info(T)` and emit code based on it.
- **`#insert(s: []u8)`** (Jai-borrowed) inserts a string of GW source at the call site.
- **`#run expr`** is a comptime call returning a value baked into the binary.

This single mechanism subsumes generics, conditional compilation, build scripts, AST macros, and serialisation codegen.

### 8.5 JIT REPL Mode

`gw repl` launches a single-task GW environment in which the user types statements and they execute. The same compiler is used; there is no separate interpreter. Top-level declarations install into a persistent symbol table for the session; bare expressions print their result.

This places GW in the company of Julia, F# Interactive, and Swift Playgrounds for interactive workflow. It is a feature absent from Rust, Zig, Odin, and Jai.

```
$ gw repl
GW repl 1.0   (std loaded)
> 5 + 7
12
> print("hello %\n", "world");
hello world
> fn fib(n: i64) -> i64 { if n < 2 then n else fib(n-1) + fib(n-2) }
> fib(20)
6765
> use std.fs;
> fs.read_to_string("readme.md") catch "(missing)"
"# GW ..."
```

---

## Part 9 — Standard Library

The standard library (`std`) is small, allocator-aware, and avoids hidden allocations.

| Module | Domain |
|---|---|
| `std.mem` | Allocators, `Arena`, slices, `mem.copy/set` |
| `std.io` | `Reader`, `Writer` traits; buffered I/O; UTF-8-safe text |
| `std.fs` | Files, paths, async file ops |
| `std.os` | Process, env, signals |
| `std.net` | Sockets, TLS, HTTP/1+2 client/server |
| `std.fmt` | `print`, `println`, `bprint` (Odin-style); the `print` function is a stdlib free function, not a language built-in |
| `std.reflect` | Runtime reflection (companion to `@type_info(T)`) |
| `std.json`, `.toml`, `.bin` | Serialisation, auto-derived from class metadata |
| `std.math`, `.simd`, `.gfx` | Vectors, matrices, quaternions, SIMD |
| `std.test` | The `test { }` runner |
| `std.task` | Tasks, channels, mutex, RWLock, `nursery` |
| `std.collections` | `Map`, `Set`, `Ring`, intrusive linked-list, handle-map |

There is **no `String` class**; `[]u8` is the universal text type with helpers in `std.text` (UTF-8-aware).

---

## Part 10 — Tooling

All tooling ships in the single `gw` binary:

| Subcommand | Purpose |
|---|---|
| `gw build` | Build the current project |
| `gw run` | Build + run |
| `gw test` | Run all `test { }` blocks |
| `gw bench` | Run all `bench { }` blocks |
| `gw fmt` | Canonical formatter (no options — Go-style) |
| `gw doc` | Generate Markdown/HTML docs from `///` comments + class metadata |
| `gw lsp` | Language server (built-in) |
| `gw repl` | JIT REPL |
| `gw pkg` | Show/install/update package dependencies |
| `gw disasm` | Disassemble emitted code |

---

## Part 11 — C Interop

```gw
extern "C" fn malloc(size: usize) -> [*]u8;
extern "C" fn free(ptr: [*]u8) -> u0;

extern "C" pub fn gw_callback(arg: [*]u8) -> i32 {
    // exposes a C-ABI symbol named gw_callback
}
```

- `extern "C"` declares the C ABI; other ABIs (`"win64"`, `"sysv"`, `"interrupt"`, `"naked"`) supported.
- `gw build --import-c foo.h` runs Clang to lower a C header to GW declarations directly (Zig's `@cImport` model). No bindings file needed.
- C structs are `extern class { ... }` with C-compatible layout.

---

## Part 12 — Inline Assembly — `asm` Blocks

Inline assembly with named-local visibility, equivalent to Rust's `asm!`, Zig's `asm`, and GCC extended asm:

```gw
fn rdtsc() -> u64 {
    let lo: u32 = 0;
    let hi: u32 = 0;
    asm {
        RDTSC
        MOV [&lo], EAX
        MOV [&hi], EDX
    }
    return (hi as u64) << 32 | (lo as u64);
}

#[interrupt]
fn irq_keyboard() -> u0 unsafe {
    asm {
        IN  AL, 0x60
        ...
        IRET
    }
}
```

- `asm` blocks accept platform-canonical mnemonics (Intel syntax on x86, ARM syntax on AArch64).
- GW locals visible by name; register pinning via `let r15 reg(R15) i: i64 = 5` for tight integration.
- `#[interrupt]` and `#[naked]` function attributes are standard low-level concerns (GCC/Clang extensions, Rust equivalents).
- `lock { ... }` block-prefixes apply LOCK semantics across all atomic ops within.

---

## Part 13 — Reflection — `@type_info`

```gw
class Player { hp: i32 @range(0, 100), name: []u8 @display }

fn dump[T: any](v: &T) -> u0 {
    const info = @type_info(T);
    print("%(", info.name);
    inline for f in info.fields {
        print("%=%, ", f.name, @field(v, f.name));
        if f.has_attr("display") then highlight();
    }
    print(")");
}
```

- `@type_info(T)` returns a comptime-known `TypeInfo` value with fields, sizes, alignments, attributes, and (for `enum`) variant info. Mirrors Zig's `std.builtin.Type` and Rust's reflection-via-attribute-macros.
- `@field(v, "name")` and `@call(fn, args)` close over field-name and function-name strings at comptime.
- `inline for f in info.fields { ... }` unrolls in MIR construction.

---

## Part 14 — Literate Sources

GW preserves the principle of rich content alongside code without inventing a new file format. The compiler accepts:

- `*.gw` — pure-text GW source, the universal interchange format. Any UTF-8 editor can read and edit it.
- `*.gw.md` — *literate* GW: a Markdown file containing fenced ```gw code blocks. The compiler tangles the code blocks; everything else is documentation. Integrates with the existing Markdown ecosystem (renderers, GitHub, LSPs, mdBook, Quarto, Jupyter).

For embedded media in source, GW provides `@embed_file("path/to/sprite.png")` — a comptime intrinsic that bakes the file's bytes into the binary as a `[]const u8`. Equivalent to Zig's `@embedFile` and Rust's `include_bytes!`.

---

## Part 15 — Sample Programs

### 15.1 Hello, World

```gw
print("Hello, world.\n");
```

`print` is a stdlib free function in `std.fmt`; no language built-in is involved. Top-level statement execution (no required `main()`) is preserved.

### 15.2 Structs, Generics, UFCS

```gw
class Vec3 { x: f32, y: f32, z: f32 }

fn dot(a: &Vec3, b: &Vec3) -> f32 {
    a.x * b.x + a.y * b.y + a.z * b.z
}

fn length(v: &Vec3) -> f32 {
    @import("std.math").sqrt(dot(v, v))
}

let v = Vec3{ .x = 1.0, .y = 2.0, .z = 2.0 };
print("length = %\n", v.length());   // UFCS: v.length() == length(&v)
```

### 15.3 Sum Types and Error Handling

```gw
enum Tile { Floor, Wall, Door{ locked: bool } }

enum MapError { OutOfBounds, MalformedFile }

fn tile_at(map: &Map, x: i32, y: i32) -> !MapError Tile {
    if x < 0 or y < 0 or x >= map.w or y >= map.h
        then return MapError.OutOfBounds;
    return map.cells[y * map.w + x];
}

fn describe(map: &Map, x: i32, y: i32) -> u0 {
    let t = tile_at(map, x, y) catch |e| match e {
        MapError.OutOfBounds => { print("off-map\n"); return; },
        else => panic("map error: %", e),
    };
    match t {
        Tile.Floor          => print("open floor\n"),
        Tile.Wall           => print("stone wall\n"),
        Tile.Door{locked}   => print("% door\n", if locked then "locked" else "open"),
    }
}
```

### 15.4 Generics with Trait Constraint

```gw
trait Hash {
    fn hash(self: &Self) -> u64;
    fn equals(self: &Self, other: &Self) -> bool;
}

class HashMap[K: any + Hash, V: any] {
    buckets: [dyn]?Pair[K, V],
    allocator: Allocator,
}

impl[K: any + Hash, V: any] HashMap[K, V] {
    fn insert(self: &mut Self, k: K, v: V) -> !u0 {
        let h = k.hash();
        // ... open-addressing logic
    }
}
```

### 15.5 Memory Management with Arena

```gw
fn load_level(path: []u8) -> !LoadError Level {
    let arena = VirtualArena.new();
    errdefer arena.deinit();   // free on error path only

    using_context(.{ .allocator = arena.allocator() }) {
        let data  = try fs.read_all(path);
        let level = try json.parse[Level](data);
        // every allocation inside parse() flowed into the arena
        return Level{ .data = level, .arena = arena };
    }
}

fn unload_level(l: &mut Level) -> u0 {
    l.arena.deinit();   // bulk free — no per-field bookkeeping
}
```

The Odin pattern: arenas express common-lifetime grouping without RAII or borrow checking.

### 15.6 Concurrency

```gw
fn pipeline(urls: []const []u8) -> !u0 {
    let work    = channel[[]u8].bounded(32);
    let pages   = channel[Page].bounded(32);

    nursery {
        // producer
        task {
            for u in urls { work <- u; }
            work.close();
        };

        // 8 fetchers
        for _ in 0..8 {
            task {
                for u in work {
                    let page = http.get(u) catch continue;
                    pages <- page;
                }
            };
        }

        // single consumer
        task {
            for p in pages { index(p); }
        };
    }   // nursery waits for all tasks here
}
```

### 15.7 FFI with C

```gw
extern "C" fn SDL_Init(flags: u32) -> i32;
extern "C" fn SDL_Quit() -> u0;

const SDL_INIT_VIDEO: u32 = 0x20;

fn main() -> !u0 {
    if SDL_Init(SDL_INIT_VIDEO) != 0 {
        return try InitError.SdlFailed;
    }
    defer SDL_Quit();
    // ...
}
```

### 15.8 Compile-Time Codegen

```gw
fn make_serialiser[T: any]() -> fn(&T, &mut Writer) -> !u0 {
    return comptime {
        const info = @type_info(T);
        let buf: [dyn]u8 = .{};
        buf.push("fn(v: &T, w: &mut Writer) -> !u0 { try w.put(\"{\");");
        inline for f, i in info.fields {
            if i > 0 { buf.push("try w.put(\",\");"); }
            buf.push_fmt("try w.put(\"%=\");", f.name);
            buf.push_fmt("try write(&v.%, w);", f.name);
        }
        buf.push("try w.put(\"}\"); return; }");
        #insert(buf.to_slice())
    };
}
```

This single comptime mechanism handles serialisers, parsers, command tables, jump tables, and SoA struct generation.

---

## Part 16 — Design Lineage

Every significant GW feature has a documented modern ancestor. The table below makes the lineage explicit.

| GW feature | Ancestor | Notes |
|---|---|---|
| Fast compilation target (1M LoC/s/core) | Jai, TPDE (Schwarz et al. 2025) | Hard fallback: ≥4× LLVM `-O0` |
| Single-pass front-end with backpatching | Pascal, Turbo Pascal, D, Crystal | Implementation strategy, not philosophy |
| No headers, no preprocessor | Standard in every modern language since ~1995 | Not novel |
| `u0` zero-size unit | Rust `()`, Zig `void` | Standard modern language design |
| `i8..i64`, `u8..u64`, arbitrary-bit-width ints | Rust, Zig, Odin | |
| `comptime` | Zig, Jai `#run` | Single mechanism for all metaprogramming |
| `#insert` and `#run` | Jai | Compile-time codegen |
| `enum` sum types | Rust `enum`, Zig `union(enum)`, ML | First-class ADTs are non-negotiable |
| Pattern matching with exhaustiveness check | Rust, Swift, ML | |
| `?T` optional, non-null `&T` | Rust, Zig, Swift, Kotlin | Eliminates null-deref class |
| `!T` error union with `try` | Zig | Cleanest known error model |
| Allocator-as-parameter + implicit `context` | Zig + Odin + Jai | Best-of-both |
| Arenas as primary discipline | Odin | Practical alternative to borrow checking for non-tree data |
| `defer` / `errdefer` (no RAII) | Zig | Avoids hidden control flow |
| Lifetime-checked `&T`, opt-out `manual`, opt-out `unsafe` | Rust softened | Safety as a gradient |
| `task` / `nursery` structured concurrency | Trio, Swift, Kotlin coroutines, Go | |
| `lock { }` blocks | Java `synchronized`, C# `lock` | Standard concurrency primitive |
| `asm { }` inline assembly | Rust `asm!`, Zig `asm`, GCC extended asm | |
| `#[interrupt]`, `#[naked]` | GCC, Clang, Rust | Standard low-level attributes |
| Class field metadata (`@range`, `@serialize`) | Rust attribute macros, C# attributes, Java annotations | |
| Built-in cross-compilation | Zig | Modern table stakes |
| Built-in formatter, LSP, doc gen | Go, Rust, Zig | Tooling included by default |
| Markdown literate sources (`*.gw.md`) | mdBook, Quarto, Jupyter, literate programming tradition (Knuth 1984) | |
| `@embed_file()` for embedded data | Zig `@embedFile`, Rust `include_bytes!` | |
| C ABI as default `extern` | Standard for systems languages | Interop is the path of adoption |
| JIT REPL with persistent symbol table | Julia, F# Interactive, Swift Playgrounds | Absent from Rust, Zig, Odin, Jai |
| UFCS dispatch | Nim, Odin, D | |

---

## Part 17 — Competitive Position

| Axis | Where GW lands |
|---|---|
| vs. **Rust** | Faster compilation, simpler borrow checker, JIT REPL, true comptime metaprogramming; gives up Polonius-grade lifetime precision and Rust's mature ecosystem |
| vs. **Zig** | Adds memory safety as default, sum types, traits, JIT REPL; gives up Zig's radical simplicity |
| vs. **C++** | Sane defaults, no headers, no preprocessor, modern type system, fast compilation; gives up 40 years of inertia |
| vs. **Odin** | Adds borrow checking, error unions, JIT REPL, Rust-grade traits; gives up Odin's deliberate simplicity |
| vs. **Jai** | Open development, borrow checker, trait-style polymorphism; gives up some of Jai's metaprogramming reach (Jai's `#run` permits I/O at compile time) |

**Competitive claim.** GW is the language for someone who wants Zig's compile speed and comptime, plus Rust's safety story (lighter), plus a JIT REPL that none of them have, plus Odin's allocator pragmatism. That position is defensible — it occupies a niche none of the four current contenders fully cover.

---

## Part 18 — What GW Deliberately Omits

To preserve speed-first, low-complexity goals, GW rejects several mainstream features:

- No exceptions, no stack unwinding. `try`/`catch` only.
- No RAII destructors. `defer`/`errdefer` only.
- No operator overloading on user types, except for a small whitelist (`Add`, `Sub`, `Mul`, `Div`, `Eq`, `Ord`, `Hash`, `Display`) tied to traits.
- No implicit conversions between numeric types (Zig rule).
- No inheritance. Composition + traits.
- No GC, no ARC. Allocators + arenas + lifetime checks.
- No closures that capture by reference across `task` boundaries without `Send`-derivation.
- No global mutable state without `unsafe`.
- No procedural macros (everything is `comptime`).
- No reflection at runtime by default — `@type_info(T)` is comptime; runtime type info costs an explicit `dyn` annotation.

---

## Part 19 — Future Research Directions

Two research items are explicitly on the post-1.0 roadmap. They are not committed work but represent the language design directions GW will track.

### 19.1 Effects / Capability System

Modern systems languages (Koka, Roc, Lean 4, Hylo) are converging on effect tracking — knowing statically which functions can allocate, perform I/O, panic, or block. GW already has the seeds of this: `Send`/`Sync` auto-derivation, `#[no_alloc]`, `#[no_comptime]`, the `unsafe` tier, and the implicit `context` parameter. A unified effect system would make these explicit and composable.

The research question: can effects be inferred (no annotation burden) while still surfacing meaningfully in API documentation and type signatures? Koka's row-polymorphic effects and Roc's `!`-suffix effect marker are the two most promising prior designs. References: *Algebraic Effects for the Rest of Us* (Brachthäuser et al., 2020); *Effekt: Capability-Passing Style for Type- and Effect-Safe, Extensible Functional Programming* (Brachthäuser et al., 2022).

### 19.2 Linear Types

GW's borrow checker is *affine* — every value is used zero or one times. *Linear* types require values to be used exactly once, which catches a class of resource-cleanup bugs that even Rust misses (e.g., forgetting to consume a `Result`, dropping a file handle without explicit close). Hylo, Austral, and the Mojo design papers explore linear types as a complement to or replacement for affine ownership.

The research question: can linear types be added to GW as an opt-in `#[must_use]`-on-steroids attribute without forking the type system? The `#[must_use]` attribute is a partial answer; full linear types would track use through pattern destructuring and partial moves.

---

## Closing

GW occupies a deliberate position in the modern systems-language design space. It synthesizes Zig's comptime + cross-compilation + bundled-libc story, Odin's pragmatic context system + allocator-explicit memory model, Rust's borrow-checked safety + trait system + ecosystem maturity, and Jai's compile-speed thesis. Its singular addition is a JIT REPL with a persistent symbol table — a productivity feature absent from every primary competitor, made possible by the same fast-compilation backend that drives the AOT story.

The technical core — three-IR pipeline, region-based borrow checking, comptime stack VM, dual-backend codegen, M:N work-stealing scheduler, bundled-libc cross-compilation — stands on its own merits. Predictable, plain-English naming throughout the language and toolchain reduces time-to-fluency for human readers and improves first-shot correctness for code-generating language models.

Two research directions are openly acknowledged: effect tracking and linear types. Both are post-1.0 concerns; both represent where 2026-and-beyond systems language design is converging. GW is positioned to adopt the resolved versions of those research questions when they arrive.
