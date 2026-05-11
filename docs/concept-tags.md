# GW Concept Tags Vocabulary

Version: 1.0-draft
Status: Pre-Phase-9 contract.
Scope: The closed vocabulary of identifiers used in the `related_concepts` field of every diagnostic (see `docs/diagnostic-format.md` §5.2).

## 1. Purpose

A concept tag is a stable, kebab-case identifier that names a single GW language feature, type, or analysis. Diagnostics carry zero or more tags in their `related_concepts` field so that LLM consumers can fetch the underlying reference page and ground their understanding before suggesting a fix.

Tags are distinct from error codes:

- An **error code** (`E4017`) identifies a specific failure mode and is documented at `https://gw-lang.org/errors/<code>`.
- A **concept tag** (`borrow-check`) identifies the underlying language feature and is documented at `https://gw-lang.org/ref/<tag>`.

One error code typically references one to four concept tags. One concept tag is referenced by many error codes.

## 2. URL Convention

Every tag in this vocabulary resolves to a permanent URL of the form:

```
https://gw-lang.org/ref/<tag>
```

The URL serves the canonical reference page for the concept. The page is structured as: one-line summary, prose explanation, code examples, related concepts, related error codes. The page is the authoritative reference; this file is the index.

## 3. Stability

Once a tag ships in a released `gw` version:

1. The tag identifier does not change.
2. The URL resolves.
3. The concept the tag names does not narrow (the explanation may broaden; the named feature does not change identity).

Renaming a tag is a breaking change. If a concept needs renaming, the old tag becomes inert (URL serves a redirect notice for two minor versions, then 410 Gone) and a new tag is introduced.

## 4. Adding New Tags

A new tag is added by:

1. Opening an RFC referencing the diagnostic(s) that need it.
2. Choosing a kebab-case name from one of the topic sections below.
3. Drafting the reference page.
4. Updating this file in the same PR as the compiler change that emits the tag.

Tags MUST NOT be emitted by the compiler before they appear in this file.

## 5. The Vocabulary

Tags are grouped by topic. The grouping is for readers of this document; tags themselves are flat — no hierarchy, no namespacing.

### 5.1 Lexical and Syntax

| Tag | Concept |
|---|---|
| `string-literals` | `"..."`, escape sequences, UTF-8 encoding rules. |
| `raw-strings` | `\\multi\nline\\` literals; no escape processing. |
| `byte-strings` | `c"..."` null-terminated literals; produce `[*:0]u8`. |
| `character-literals` | `'A'` (rune) and `c'A'` (u8) literals. |
| `numeric-literals` | Integer and float literals; underscores as separators; suffixes. |
| `comments` | `//`, `/* */`, `///` doc-comment forms. |
| `identifiers` | Identifier syntax; ASCII; case sensitivity. |
| `keywords` | The reserved-word set; tokens that cannot be used as identifiers. |
| `statement-terminators` | Semicolon rules; trailing-semicolon optionality. |

### 5.2 Types — Primitives

| Tag | Concept |
|---|---|
| `primitive-types` | The set of built-in scalar types. |
| `unit-type` | `u0` and its semantics. |
| `bool-type` | `bool`, `true`, `false`. |
| `integer-types` | `iN`/`uN` for fixed widths; sign semantics; overflow. |
| `arbitrary-bit-integers` | `iN`/`uN` for arbitrary N in 1..256. |
| `floating-point` | `f16`, `bf16`, `f32`, `f64`; IEEE-754 semantics. |
| `rune-type` | The Unicode scalar value type. |
| `nil-type` | The type of `nil`; assignability into `?T`. |
| `pointer-sized-ints` | `usize`, `isize`. |

### 5.3 Types — Composites

| Tag | Concept |
|---|---|
| `classes` | The aggregate-POD class form; `class Foo { ... }`. |
| `packed-classes` | `packed class` — bit-packed, no padding. |
| `extern-classes` | `extern class` — C-ABI guaranteed layout. |
| `field-metadata` | `@range`, `@serialize`, etc.; preserved through compilation. |
| `field-access` | Reading and writing class fields. |
| `enums` | Tagged-union sum types; payload variants. |
| `enum-variants` | Individual `enum` variants; payload shapes. |
| `tuples` | Anonymous fixed-arity products. |
| `anonymous-aggregates` | `.{ .x = 1, .y = 2 }`; type-from-context syntax. |

### 5.4 References, Pointers, Slices

| Tag | Concept |
|---|---|
| `references` | `&T`, non-null borrowed references. |
| `mutable-references` | `&mut T`, exclusive borrowed references. |
| `raw-pointers` | `*T`; nullable; `unsafe`-only. |
| `many-pointers` | `[*]T`, the C-style pointer-to-array. |
| `sentinel-pointers` | `[*:0]u8`, sentinel-terminated pointers. |
| `slices` | `[]T`; the universal sequence type. |
| `fixed-arrays` | `[N]T`; size-known arrays. |
| `dynamic-arrays` | `[dyn]T`; allocator-aware growable arrays. |
| `optional-types` | `?T`; absence-as-type. |
| `dereferencing` | `*expr` and `**expr` in expression position. |

### 5.5 Generics

| Tag | Concept |
|---|---|
| `generics` | Comptime parametric polymorphism. |
| `type-parameters` | `[T: Trait]` syntax; introduction sites. |
| `trait-bounds` | `T: Trait1 + Trait2`; constraint syntax. |
| `any-bound` | The unconstrained `any` bound; root of the trait lattice. |
| `monomorphization` | Per-instantiation specialization. |
| `shared-monomorphization` | Vtable-parameterized polymorphic bodies; `#[shared]`. |
| `comptime-generic-args` | Passing values as generic args; `[N: comptime usize]`. |
| `where-clauses` | `where T: Trait` bound syntax on impls and fns. |

### 5.6 Traits and Impls

| Tag | Concept |
|---|---|
| `traits` | The `trait` declaration form. |
| `impl-blocks` | `impl Trait for Type { ... }`. |
| `trait-resolution` | The constraint-solving pass. |
| `trait-coherence` | One impl per (trait, type); orphan rules. |
| `orphan-rules` | The locality constraint on impl placement. |
| `default-methods` | Trait-provided method bodies; per-impl override. |
| `dynamic-dispatch` | `dyn Trait`; vtable-based calls. |
| `static-dispatch` | Monomorphized calls; default for generics. |
| `auto-derived-traits` | `Send`, `Sync`, `Copy`; structurally inferred. |
| `display-trait` | The standard formatting trait. |
| `ord-trait` | The standard ordering trait. |
| `hash-trait` | The standard hashing trait. |
| `iterator-trait` | The standard iteration trait. |
| `reader-trait` | The standard byte-input trait. |
| `writer-trait` | The standard byte-output trait. |

### 5.7 Memory and Ownership

| Tag | Concept |
|---|---|
| `ownership` | The single-owner discipline for owned values. |
| `move-semantics` | Default move-on-assignment for non-`Copy` types. |
| `copy-semantics` | Trivially-copyable types; the `Copy` trait. |
| `safety-tiers` | The three tiers: `safe`, `manual`, `unsafe`. |
| `safe-tier` | The default safety tier; borrow-checked. |
| `manual-tier` | Opt-out from aliasing check; retain null/bounds checks. |
| `unsafe-tier` | Full opt-out; raw pointers, asm, manual layout. |
| `unsafe-blocks` | `unsafe { ... }` scoped opt-out. |
| `allocators` | The `Allocator` trait and implementations. |
| `context-allocator` | The implicit `context.allocator` mechanism. |
| `arenas` | Bump-arena allocators; bulk free. |
| `virtual-arenas` | Mmap-reserved, page-committed growing arenas. |
| `defer` | `defer` statement; reverse-order scope exit. |
| `errdefer` | `errdefer` statement; error-only scope exit. |

### 5.8 Borrow Checking

| Tag | Concept |
|---|---|
| `borrow-check` | The borrow-checker analysis pass. |
| `region-inference` | The lifetime-region inference algorithm. |
| `lifetime-annotations` | Explicit `'a` annotations (rare in GW; mostly elided). |
| `lifetime-elision` | The elision rules that hide lifetime params. |
| `aliasing-rules` | One-mut-or-many-shared invariant. |
| `mutable-borrow` | `&mut` borrows and their exclusion rules. |
| `shared-borrow` | `&` borrows and their compatibility. |
| `borrow-scope` | Lexical scope as borrow-lifetime boundary. |
| `move-after-borrow` | Use-after-borrow class of errors. |
| `use-after-move` | Use-after-move class of errors. |
| `borrow-across-await` | Borrow validity across async suspension. |

### 5.9 Error Handling

| Tag | Concept |
|---|---|
| `error-unions` | `!E T` types. |
| `error-sets` | Closed/open unions of error variants. |
| `try-propagation` | The `try` prefix operator. |
| `catch-recovery` | The `catch |e| ...` form. |
| `must-not-fail` | The `!!` unwrap-or-panic operator. |
| `error-return-traces` | Debug-build error origin chains. |

### 5.10 Concurrency

| Tag | Concept |
|---|---|
| `tasks` | The `task` spawn form. |
| `channels` | `channel<T>`; bounded MPMC. |
| `async-await` | `async fn`, `await`; state-machine transform. |
| `nurseries` | `nursery { ... }`; structured concurrency. |
| `send-sync` | The `Send` and `Sync` auto-traits. |
| `lock-blocks` | `lock { ... }` mutex acquisition. |
| `atomic-types` | `atomic[Order] T`. |
| `memory-ordering` | `Relaxed`, `Acquire`, `Release`, `AcqRel`, `SeqCst`. |
| `cancellation` | Nursery-driven task cancellation. |

### 5.11 Comptime and Reflection

| Tag | Concept |
|---|---|
| `comptime` | Compile-time execution. |
| `comptime-evaluation` | The stack-VM that runs comptime code. |
| `comptime-types` | `type` as a first-class comptime value. |
| `comptime-int` | Width-flexible integer literals; coercion. |
| `comptime-float` | Width-flexible float literals; coercion. |
| `run-directive` | `#run expr`; comptime call returning a constant. |
| `insert-directive` | `#insert(s)`; source-string injection. |
| `inline-for` | `inline for ... in ...`; comptime unrolling. |
| `type-info` | `@type_info(T)`; structural type introspection. |
| `field-intrinsic` | `@field(v, "name")`; comptime field access. |
| `call-intrinsic` | `@call(fn, args)`; comptime call construction. |
| `embed-file` | `@embed_file(path)`; comptime file embedding. |
| `comptime-sandbox` | Comptime I/O, memory, operation caps. |
| `operation-budget` | The configurable comptime-step cap. |
| `memory-budget` | The configurable comptime-allocation cap. |

### 5.12 Pattern Matching

| Tag | Concept |
|---|---|
| `pattern-matching` | The `match` form generally. |
| `pattern-exhaustiveness` | The exhaustiveness check. |
| `irrefutable-patterns` | Patterns that match unconditionally; usable in `let`. |
| `refutable-patterns` | Conditional patterns; usable in `match`, `if let`. |
| `or-patterns` | `A | B` patterns. |
| `range-patterns` | `0..10` and `0..=10` patterns. |
| `binding-patterns` | Identifiers in patterns; binding semantics. |
| `wildcard-pattern` | `_` in patterns. |
| `struct-patterns` | Destructuring class fields. |
| `enum-patterns` | Destructuring enum variants. |

### 5.13 Modules and Visibility

| Tag | Concept |
|---|---|
| `modules` | The `mod` declaration and module graph. |
| `imports` | The `use` declaration. |
| `visibility` | `pub` and the default-private rule. |
| `module-graph` | The cross-module dependency DAG. |
| `forward-references` | Out-of-order definitions within a module. |
| `single-pass-compilation` | The signature-first / bodies-second discipline. |

### 5.14 FFI and ABI

| Tag | Concept |
|---|---|
| `extern-functions` | `extern "C" fn ...` declarations. |
| `c-abi` | The C calling convention; `extern "C"` semantics. |
| `abi-specifications` | `"win64"`, `"sysv"`, `"interrupt"`, `"naked"` ABI strings. |
| `import-c` | `gw build --import-c`; clang-driven header lowering. |
| `c-layout` | The `extern class` layout guarantee. |

### 5.15 Inline Assembly and Low-Level

| Tag | Concept |
|---|---|
| `inline-assembly` | `asm { ... }` blocks. |
| `register-pinning` | `reg(RAX)` annotations on locals. |
| `naked-functions` | `#[naked]` attribute. |
| `interrupt-handlers` | `#[interrupt]` attribute. |
| `no-alloc` | `#[no_alloc]` attribute. |
| `must-use` | `#[must_use]` attribute. |

### 5.16 Tooling

| Tag | Concept |
|---|---|
| `documentation-comments` | `///` doc-comment processing. |
| `doctest` | Runnable examples in doc comments. |
| `package-resolution` | The PubGrub-based dependency resolver. |
| `semver` | Semantic-versioning rules in `build.gw`. |
| `build-script` | `build.gw` as executable build manifest. |
| `literate-source` | `*.gw.md` fenced-block extraction. |

## 6. Index by Compiler Phase

The compiler phase that typically emits a tag, for reference:

- **Lexer / parser**: every tag in §5.1 plus `keywords`, `statement-terminators`.
- **Resolver**: `modules`, `imports`, `visibility`, `forward-references`, `single-pass-compilation`.
- **Type checker**: every tag in §5.2–§5.6 plus pattern tags.
- **Trait resolution**: every tag in §5.6.
- **Borrow checker**: every tag in §5.8 plus `defer`, `errdefer`, `unsafe-blocks`, `ownership`, `move-semantics`.
- **Comptime engine**: every tag in §5.11.
- **MIR / codegen / linker**: `c-abi`, `abi-specifications`, `extern-functions`, `inline-assembly`, register/interrupt/naked attributes.
- **Tooling**: every tag in §5.16.

A diagnostic MAY reference tags outside its emitting phase when the underlying concept spans phases. Example: an `E4017` borrow-check error can reference `borrow-check`, `region-inference`, and `mutable-references` together.

## 7. Non-Goals

- **Hierarchy.** Tags are flat. No `memory/borrow-check`; just `borrow-check`.
- **Synonyms.** One concept, one tag. Aliases create ambiguity.
- **Tooling tags for IDE features.** Concept tags name language features, not editor capabilities.
- **Localization.** Tag identifiers are English-only. Localized reference pages may exist; the tag identifier is the URL key.
