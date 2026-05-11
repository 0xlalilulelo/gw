# GW Diagnostic Format Specification

Version: 1.0-draft
Status: Pre-Phase-9 contract — implementation tracks this document.
Scope: The wire format and stability contract for every diagnostic emitted by `gw`, every tool built on it, and every consumer of the LSP.

## 1. Goals

1. **One format, three audiences.** Humans reading a terminal, LSP clients in an IDE, and code-generating language models all consume the same diagnostic stream. Each gets the projection of the format it needs.

2. **Stable error codes with permanent documentation.** Every diagnostic carries an `E0042`-style code. The code maps to a permanent documentation URL. Codes never change meaning once stabilized.

3. **Suggestions are machine-applicable.** A fix attached to a diagnostic carries an `Applicability` tier that says whether a tool can apply it without human review.

4. **Diagnostics are self-contained.** A consumer reading a single diagnostic record can apply or display the fix without re-opening the source file. Source byte ranges are absolute, suggested replacements are textual, and a small repro snippet is included for context.

5. **No hidden state.** The compiler emits one diagnostic record at a time, line-delimited JSON, no out-of-band session state.

## 2. Diagnostic Levels

Four levels, ordered by severity:

| Level   | Meaning                                                                                  |
|---------|------------------------------------------------------------------------------------------|
| `error` | Compilation fails. Output binary is not produced.                                        |
| `warn`  | Compilation succeeds. The default profile elevates these to errors in `--release`.       |
| `note`  | Informational. Attached to a parent error/warn; never appears alone.                     |
| `help`  | Suggested action. Attached to a parent error/warn; usually carries `suggestion`.         |

`note` and `help` records are always child records of an `error` or `warn`. They never stream independently.

## 3. Wire Format

### 3.1 Encoding

- **Transport:** Line-delimited JSON (one JSON object per line; `\n` terminated; UTF-8).
- **Stream:** stderr (stdout is reserved for program output in `gw run`).
- **Order:** Diagnostics arrive in source order within a file. Across files, order is unspecified.
- **Termination:** End-of-stream is EOF on stderr. There is no "end of diagnostics" marker.

### 3.2 Top-Level Diagnostic Object

```json
{
  "schema_version": "1.0",
  "level": "error",
  "code": "E4017",
  "doc_url": "https://gw-lang.org/errors/E4017",
  "message": "cannot borrow `*v` as mutable because it is also borrowed as immutable",
  "primary_span": { ... },
  "secondary_spans": [ { ... } ],
  "children": [
    { "level": "note", "message": "immutable borrow occurs here", "primary_span": { ... } },
    { "level": "help", "message": "consider scoping the immutable borrow with a block",
      "primary_span": { ... },
      "suggestion": { ... } }
  ],
  "minimal_repro": "let v: [dyn]i32 = .{1,2,3};\nlet r = &v[0];\nv.push(4);\nprint(\"%\", r);",
  "related_concepts": ["borrow-check", "region-inference"],
  "compiler_version": "0.4.2-dev"
}
```

Every field is required unless marked optional below:

| Field              | Type            | Notes                                                                                  |
|--------------------|-----------------|----------------------------------------------------------------------------------------|
| `schema_version`   | string          | Semver of this format. Bumped only for incompatible changes.                            |
| `level`            | enum            | `"error"`, `"warn"`, `"note"`, `"help"`.                                                |
| `code`             | string \| null  | Stable error code; `null` only for `note`/`help` children.                              |
| `doc_url`          | string \| null  | `https://gw-lang.org/errors/<code>`; null when `code` is null.                          |
| `message`          | string          | Single-line, no trailing punctuation, no terminal colors.                               |
| `primary_span`     | Span            | The locus of the diagnostic.                                                            |
| `secondary_spans`  | [SecondarySpan] | Zero or more related ranges with labels.                                                |
| `children`         | [Diagnostic]    | Nested `note`/`help` records. Recursive; children have `code: null`.                    |
| `minimal_repro`    | string \| null  | Optional; a small, self-contained GW snippet that triggers the same diagnostic. See §5.1. |
| `related_concepts` | [string]        | Optional; kebab-case concept tags into the language reference. See §5.2.                |
| `compiler_version` | string          | Semver-with-suffix; identifies the producing `gw`.                                      |

### 3.3 Span Object

```json
{
  "file": "src/parser.gw",
  "byte_start": 1842,
  "byte_end": 1855,
  "line_start": 73,
  "line_end": 73,
  "col_start": 12,
  "col_end": 25
}
```

Bytes are 0-indexed half-open `[start, end)`. Lines and columns are 1-indexed (matching every editor) and are derived from bytes. UTF-8 bytes are the indexing unit; columns are *display columns* (grapheme cluster boundaries) for human display, and additionally as `byte_col_start`/`byte_col_end` for editors that need byte offsets. (LSP wire format uses different conventions and the LSP server translates.)

### 3.4 SecondarySpan Object

```json
{
  "span": { ... },
  "label": "value first borrowed here",
  "style": "secondary"
}
```

`style` is one of:
- `secondary` — highlights the conflict source (the most common case).
- `context` — provides framing without highlighting (e.g., "earlier in this expression").

`primary` is reserved for the top-level `primary_span` and never appears in `secondary_spans`.

### 3.5 Suggestion Object

Attached to `help` children when an automatic fix is available:

```json
{
  "applicability": "machine_applicable",
  "edits": [
    {
      "span": { ... },
      "replacement": "v.clone()"
    }
  ],
  "rendered": "v.clone()"
}
```

| Field           | Type     | Notes                                                |
|-----------------|----------|------------------------------------------------------|
| `applicability` | enum     | See §3.6.                                            |
| `edits`         | [Edit]   | Atomic; tools apply all or none.                     |
| `rendered`      | string   | Human-readable preview text.                         |

### 3.6 Applicability Levels

Four levels, semantically identical to rustc's:

| Level                | Meaning                                                                                              |
|----------------------|------------------------------------------------------------------------------------------------------|
| `machine_applicable` | Apply without human review. Syntactically valid, semantically equivalent (or strictly more correct). |
| `has_placeholders`   | Apply with substitution. Contains `<placeholder>` strings the user must fill in.                     |
| `maybe_incorrect`    | Display to the user; do not auto-apply. The fix may be wrong, or compile but be wrong.               |
| `unspecified`        | Default when the diagnostic doesn't provide guidance.                                                |

Tools that auto-apply suggestions (LSP code actions, `gw fmt --fix`, AI assistants) MUST check this field. The contract: a `machine_applicable` fix, applied in isolation, will still compile and is at least as correct as the original code.

## 4. Error Codes

### 4.1 Numbering

Error codes follow the pattern `E[0-9]{4}` (e.g., `E0042`, `E4017`). Codes are allocated by the compiler team and are unique across the language. Codes are *not* reused; a deprecated code becomes inert but is never reassigned.

Code ranges by phase:

| Range         | Owner                                |
|---------------|--------------------------------------|
| `E0001-E0999` | Lexer & parser                       |
| `E1000-E1999` | Resolver (name resolution)           |
| `E2000-E2999` | Type checker                         |
| `E3000-E3999` | Trait resolution                     |
| `E4000-E4999` | Borrow checker                       |
| `E5000-E5999` | Comptime engine                      |
| `E6000-E6999` | MIR / lowering                       |
| `E7000-E7999` | Codegen                              |
| `E8000-E8999` | Linker / target / driver             |
| `E9000-E9999` | Reserved for tooling (LSP, doc, fmt) |

Warnings use a parallel `W[0-9]{4}` namespace with the same ranges. ICEs (internal compiler errors) are emitted as level `error` with code `E9999`.

### 4.2 Stability

Once a code is documented at `https://gw-lang.org/errors/<code>` and ships in a release, three properties hold permanently:

1. The code's meaning does not change.
2. The doc URL resolves.
3. The diagnostic with that code is emitted in semantically equivalent situations. (The specific wording may improve; the situations covered may broaden but not narrow.)

If the compiler team needs to split a diagnostic's coverage into finer categories, the original code remains as a parent; new sub-codes are allocated for the refined categories.

### 4.3 Documentation Format

Each code's documentation page contains, in order:

1. **One-line summary.**
2. **Erroneous example** (a minimal `.gw` snippet that triggers the error).
3. **Why it's wrong** (the rule being violated).
4. **How to fix** (one or more patterns, with code examples).
5. **Related codes.**
6. **Related concepts** (links into the language reference).

The `minimal_repro` field in the wire format is drawn from item 2.

## 5. Novel Fields

These two fields are not in rustc's JSON output. They exist to serve LLM consumers and rapid debugging.

### 5.1 `minimal_repro`

A small, self-contained GW snippet — typically 1–8 lines — that triggers the same diagnostic on an empty project. The snippet is paste-runnable in `gw repl` or as a top-level program.

Purpose: an AI assistant or human seeing the diagnostic can re-create it in isolation, experiment with fixes, and confirm the fix before patching the original code. This is especially valuable in long-context settings where the surrounding code is voluminous.

Not every diagnostic carries one. Lexer and parser errors typically do not (the user's snippet is already minimal). Borrow-check, type-check, and trait-resolution errors usually do.

Format: a single string, GW source, no surrounding fences. Newlines preserved.

### 5.2 `related_concepts`

A list of kebab-case tags identifying language concepts touched by the diagnostic. Tags are drawn from a closed vocabulary defined in `docs/concept-tags.md`; each tag has a stable URL into the language reference.

Examples:
- `move-semantics`
- `lifetime-annotations`
- `trait-bounds`
- `comptime-evaluation`
- `pattern-exhaustiveness`
- `region-inference`

Purpose: an LLM consumer can fetch the related-concept reference pages and ground its understanding of the diagnostic before suggesting a fix. Distinct from `doc_url`, which points to the *error*; `related_concepts` point to the *underlying language features*.

## 6. CLI Flags

The driver exposes diagnostic output modes via `--diagnostic-format`:

| Flag                                | Effect                                                                       |
|-------------------------------------|------------------------------------------------------------------------------|
| (default)                           | Human-readable, ANSI-colored to stderr. No JSON.                             |
| `--diagnostic-format=json`          | Line-delimited JSON to stderr; no rendered text.                             |
| `--diagnostic-format=json-ansi`     | JSON with an additional `rendered` field holding ANSI-colored text.          |
| `--diagnostic-format=json-plain`    | JSON with an additional `rendered` field holding plain text (no ANSI).       |

The `rendered` form is a convenience for tools that want to display human-readable text without re-implementing the layout algorithm. The structured fields remain authoritative.

`--diagnostic-format=json*` implies `--color=never` for the JSON stream; the inner `rendered` field is the only place ANSI codes appear, and only when `json-ansi` is selected.

## 7. Human-Readable Format

The canonical rendering of a diagnostic. The compiler's renderer is the reference; third-party renderers SHOULD match.

Layout:

```
error[E0382]: use of moved value: `v`
   ┌─ src/parser.gw:73:12
   │
71 │     let v = String.new("hi");
   │         - value moved here
72 │     let _ = v;
73 │     print("%", v);
   │                ^ value used here after move
   │
   = note: `String` does not implement `Copy`
   = help: consider cloning the value
   │
   │ - print("%", v);
   │ + print("%", v.clone());
   │
   = related: ownership, move-semantics
   = docs: https://gw-lang.org/errors/E0382
```

Conventions:
- The error code is in square brackets after the level.
- Spans use a left gutter (line number) and a span-style ASCII art frame (`┌─`, `│`).
- Source lines around the primary span show ±2 lines of context, with the primary span marked by `^^^^` underline.
- Notes are prefixed `= note:`.
- Help messages with suggestions show a unified diff of the proposed change.
- The `docs:` line is always last.

ANSI coloring (when enabled): errors red, warnings yellow, notes blue, help cyan, spans bold.

## 8. Streaming Semantics

- Each diagnostic is one JSON object on one line.
- Compiler may emit diagnostics from multiple files interleaved when parallel compilation is enabled.
- Compiler does NOT emit a final summary record in JSON mode. Consumers count records themselves.
- Non-zero exit code if any `level: error` was emitted; zero otherwise.
- In JSON mode, no non-JSON output is written to stderr. Internal compiler errors (ICEs) are themselves emitted as diagnostics with code `E9999`.

## 9. Versioning

The format is versioned by `schema_version`. Compatible changes (adding optional fields, adding error codes, broadening levels) leave the major version unchanged. Incompatible changes (removing fields, changing field semantics, narrowing levels) bump the major version.

Consumers MUST tolerate unknown fields (forward compatibility). Consumers MAY require a specific `schema_version` and emit a warning when a newer one is encountered.

The current version is `1.0`. Pre-1.0 versions are documented as `0.x-draft` and may break without notice.

## 10. Examples

### 10.1 Borrow-check error with machine-applicable fix

```json
{"schema_version":"1.0","level":"error","code":"E4017","doc_url":"https://gw-lang.org/errors/E4017","message":"cannot borrow `*v` as mutable because it is also borrowed as immutable","primary_span":{"file":"src/main.gw","byte_start":214,"byte_end":222,"line_start":12,"line_end":12,"col_start":5,"col_end":13},"secondary_spans":[{"span":{"file":"src/main.gw","byte_start":189,"byte_end":195,"line_start":11,"line_end":11,"col_start":13,"col_end":19},"label":"immutable borrow occurs here","style":"secondary"}],"children":[{"level":"help","message":"consider scoping the immutable borrow with a block","primary_span":{"file":"src/main.gw","byte_start":189,"byte_end":195,"line_start":11,"line_end":11,"col_start":13,"col_end":19},"suggestion":{"applicability":"machine_applicable","edits":[{"span":{"file":"src/main.gw","byte_start":180,"byte_end":195,"line_start":11,"line_end":11,"col_start":4,"col_end":19},"replacement":"{ let r = &v[0]; print(\"%\", r); }"}],"rendered":"{ let r = &v[0]; print(\"%\", r); }"}}],"minimal_repro":"let v: [dyn]i32 = .{1,2,3};\nlet r = &v[0];\nv.push(4);\nprint(\"%\", r);","related_concepts":["borrow-check","region-inference"],"compiler_version":"0.4.2-dev"}
```

### 10.2 Trait-resolution error with placeholder suggestion

```json
{"schema_version":"1.0","level":"error","code":"E3014","doc_url":"https://gw-lang.org/errors/E3014","message":"the trait `Display` is not implemented for `Point`","primary_span":{"file":"src/main.gw","byte_start":342,"byte_end":350,"line_start":18,"line_end":18,"col_start":18,"col_end":26},"secondary_spans":[],"children":[{"level":"help","message":"implement `Display` for `Point`","primary_span":{"file":"src/main.gw","byte_start":12,"byte_end":12,"line_start":2,"line_end":2,"col_start":1,"col_end":1},"suggestion":{"applicability":"has_placeholders","edits":[{"span":{"file":"src/main.gw","byte_start":12,"byte_end":12,"line_start":2,"line_end":2,"col_start":1,"col_end":1},"replacement":"impl Display for Point {\n    fn fmt(self: &Self, w: &mut Writer) -> !u0 {\n        <todo: write Point fields to w>\n    }\n}\n\n"}],"rendered":"impl Display for Point { ... }"}}],"minimal_repro":"class Point { x: f32, y: f32 }\nfn main() -> u0 { let p = Point{.x=1, .y=2}; print(\"%\", p); }","related_concepts":["traits","trait-bounds","display"],"compiler_version":"0.4.2-dev"}
```

### 10.3 Lexer error (no minimal_repro)

```json
{"schema_version":"1.0","level":"error","code":"E0042","doc_url":"https://gw-lang.org/errors/E0042","message":"unterminated string literal","primary_span":{"file":"src/main.gw","byte_start":58,"byte_end":59,"line_start":3,"line_end":3,"col_start":15,"col_end":16},"secondary_spans":[],"children":[],"minimal_repro":null,"related_concepts":["string-literals"],"compiler_version":"0.4.2-dev"}
```

## 11. Test & Compliance

The `tests/diagnostics/` corpus is the compliance suite. Each entry is a `.gw` source file paired with a `.expected.jsonl` file containing the expected diagnostic stream. CI fails if any field of any diagnostic doesn't match, excluding `compiler_version`.

The schema is also published as a JSON Schema document at `docs/diagnostic-schema.json` for consumer validation.

## 12. Non-Goals

- **Performance metrics.** Timing and memory information are emitted via a separate `--metrics` channel, not in diagnostics.
- **Cross-process call traces.** Each diagnostic is local to one compilation.
- **Localization.** Messages are English-only at v1. Localization is a Phase 10+ concern and would add a `message_id` field, not change existing fields.
- **Interactive prompts.** The diagnostic stream is one-way. Tools that want interaction layer it on top.
