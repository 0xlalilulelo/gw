//! GW name resolver — Phase 1 minimum.
//!
//! See `docs/architecture.md` Part B.5 (resolver). This Phase-1 slice is
//! intentionally narrow: a single-frequency module is walked, top-level
//! function declarations are registered, and primitive type names
//! (`i32`, `bool`, `u0`, …) are resolved as built-ins. Local-scope
//! resolution, generics, and the cross-frequency graph land in later
//! increments.
//!
//! Output: a [`ResolvedModule`] containing a flat map from names to
//! [`DefId`]s plus the underlying CST root for downstream consumers.

use arsenal_ast::{AstNode, ClassDecl, FnDecl, Item, LibertyDecl, Module, SyntaxNode, UseDecl};
use arsenal_lex::{DiagBag, Diagnostic, FileId, Label, SourceMap, Span};
use rustc_hash::FxHashMap;

/// Resolver error codes. Reserved range: `E0200..E0299`.
pub mod ec {
    use arsenal_lex::ErrorCode;
    /// A name is declared more than once at the same scope.
    pub const DUPLICATE_DEFINITION: ErrorCode = ErrorCode(200);
    /// A function declaration has no name token.
    pub const MISSING_NAME: ErrorCode = ErrorCode(201);
    /// Top-level statements appear alongside an explicit `fn main`.
    /// Both would lower to the same `main` symbol.
    pub const TOP_LEVEL_STMTS_WITH_EXPLICIT_MAIN: ErrorCode = ErrorCode(202);
    /// Top-level statements appeared in a sibling `.gw` file (Phase 2
    /// increment F.1). Only the build-target file may carry top-level
    /// statements that synthesise the implicit `main`.
    pub const TOP_LEVEL_STMTS_IN_LIBRARY: ErrorCode = ErrorCode(203);
    /// `use <name>;` referenced a module that no file declared via
    /// `liberty <name>;` (Phase 2 increment F.2).
    pub const UNKNOWN_MODULE: ErrorCode = ErrorCode(204);
    /// A file declared more than one `liberty <name>;` (Phase 2
    /// increment F.2). Each file is a single module.
    pub const DUPLICATE_LIBERTY: ErrorCode = ErrorCode(205);
}

/// Stable identifier for a top-level definition within a module.
///
/// Indices are assigned in source order during resolution; the
/// allocation is deterministic for a given source file.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DefId(pub u32);

/// Kinds of top-level definitions the Phase-1 resolver recognises.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DefKind {
    /// `fn name(...) -> ... { ... }`
    Fn,
    /// `class Name { fields }` — POD aggregate type declaration.
    Class,
    /// Implicit `main` synthesised from top-level statements outside any
    /// `fn`. The `Def::syntax` field points at the [`Module`] node so
    /// downstream passes can iterate `Module::stmts()`.
    SyntheticMain,
}

/// One entry in the module symbol table.
#[derive(Clone, Debug)]
pub struct Def<'a> {
    /// Stable id within the module.
    pub id: DefId,
    /// What kind of item this is.
    pub kind: DefKind,
    /// Name as it appeared in source.
    pub name: String,
    /// Span of the name identifier (used for diagnostics).
    pub name_span: Span,
    /// Borrowed CST node for the underlying item.
    pub syntax: &'a SyntaxNode<'a>,
}

/// Result of resolving a single module.
pub struct ResolvedModule<'a> {
    /// Underlying CST root.
    pub module: Module<'a>,
    /// Definitions in source order.
    pub defs: Vec<Def<'a>>,
    /// Flat-namespace lookup from source name to [`DefId`]. Holds
    /// items from non-`liberty` files. Phase 2 F.3 keeps this map
    /// stable (independent of `use` decls in any file); cross-file
    /// imports flow through [`Self::file_scopes`].
    pub by_name: FxHashMap<String, DefId>,
    /// Per-file effective name scope (Phase 2 increment F.3). Each
    /// file's scope = the flat `by_name` pool + the file's own items
    /// (regardless of liberty) + items from modules the file
    /// `use`s. Typeck consults this map via
    /// [`Self::lookup_in_file`] so a `use foo;` in main.gw doesn't
    /// leak `foo`'s items into lib.gw.
    pub file_scopes: FxHashMap<FileId, FxHashMap<String, DefId>>,
}

impl<'a> ResolvedModule<'a> {
    /// Locate a definition by source name in the flat global pool.
    /// Backwards-compat entry point for callers that don't have a
    /// file context (single-file builds, AST tests, …). Phase 2 F.3
    /// callers with a known call-site file should prefer
    /// [`Self::lookup_in_file`].
    pub fn lookup(&self, name: &str) -> Option<&Def<'a>> {
        self.by_name.get(name).map(|id| &self.defs[id.0 as usize])
    }

    /// Locate a definition visible in `file`'s effective scope. Falls
    /// back to the flat namespace if `file` doesn't have a recorded
    /// scope (e.g., a default-constructed `FileId::NONE`), so
    /// existing single-file callers stay sound.
    pub fn lookup_in_file(&self, file: FileId, name: &str) -> Option<&Def<'a>> {
        if let Some(scope) = self.file_scopes.get(&file) {
            if let Some(id) = scope.get(name) {
                return Some(&self.defs[id.0 as usize]);
            }
            return None;
        }
        self.lookup(name)
    }
}

/// Resolve a parsed [`Module`].
///
/// The Phase-1 walker registers every top-level [`Item::Fn`]; other
/// item kinds are ignored at this layer (they were already flagged by
/// the parser's "not yet supported" diagnostics). Duplicate names emit
/// a [`Diagnostic`] but do not abort: the second definition is recorded
/// at a fresh `DefId` so downstream passes can still proceed.
pub fn resolve_module<'a>(
    module_node: &'a SyntaxNode<'a>,
    sm: &SourceMap,
    diags: &mut DiagBag,
) -> ResolvedModule<'a> {
    resolve_modules(module_node, &[], sm, diags)
}

/// Resolve a parsed primary [`Module`] together with zero or more
/// secondary modules (Phase 2 increment F.1). All items from every
/// module land in a single flat namespace; duplicate names diagnose
/// regardless of which file the second definition came from. Only
/// the primary module may carry top-level statements (the synthetic
/// `main`); top-level statements in secondary modules diagnose with
/// `TOP_LEVEL_STMTS_IN_LIBRARY`.
pub fn resolve_modules<'a>(
    primary_node: &'a SyntaxNode<'a>,
    extra_nodes: &[&'a SyntaxNode<'a>],
    sm: &SourceMap,
    diags: &mut DiagBag,
) -> ResolvedModule<'a> {
    let module =
        Module::cast(primary_node).expect("resolve_modules called on non-Module syntax node");
    let mut defs: Vec<Def<'a>> = Vec::new();
    let mut by_name: FxHashMap<String, DefId> = FxHashMap::default();
    // Phase 2 increment F.2: each file with a `liberty foo;` declaration
    // contributes to `module_tables[foo]` instead of the global flat
    // `by_name`. Phase 2 increment F.3: `use foo;` decls populate
    // per-file scopes (in `file_uses`), not the global by_name.
    let mut module_tables: FxHashMap<String, FxHashMap<String, DefId>> = FxHashMap::default();
    // Per-file own-item lists (Phase 2 F.3). Tracks every file's own
    // items so the post-pass can build effective scopes that include
    // both the flat pool and the file's own definitions.
    let mut file_items: FxHashMap<FileId, Vec<(String, DefId)>> = FxHashMap::default();
    // Per-file `use` decls (Phase 2 F.3). Module-name + the use-decl's
    // span (for diagnostics).
    let mut file_uses: FxHashMap<FileId, Vec<(String, Span)>> = FxHashMap::default();
    // The set of files we've seen at all (so the post-pass knows which
    // file_scopes entries to build, even for files that only have a
    // `liberty` decl and no items / uses).
    let mut all_files: Vec<FileId> = Vec::new();

    process_module(
        module,
        primary_node,
        sm,
        &mut defs,
        &mut by_name,
        &mut module_tables,
        &mut file_items,
        &mut file_uses,
        &mut all_files,
        /* is_primary */ true,
        diags,
    );

    for &extra_node in extra_nodes {
        let Some(extra_module) = Module::cast(extra_node) else {
            continue;
        };
        process_module(
            extra_module,
            extra_node,
            sm,
            &mut defs,
            &mut by_name,
            &mut module_tables,
            &mut file_items,
            &mut file_uses,
            &mut all_files,
            /* is_primary */ false,
            diags,
        );
    }

    // Phase 1 increment 11a: top-level statements outside any `fn`
    // synthesise an implicit `main`. If the user also wrote an explicit
    // `fn main`, the two would collide on the linker symbol — diagnose
    // and skip the synthetic registration.
    if module.stmts().next().is_some() {
        if let Some(&prev_id) = by_name.get("main") {
            let prev = &defs[prev_id.0 as usize];
            diags.push(
                Diagnostic::error(
                    ec::TOP_LEVEL_STMTS_WITH_EXPLICIT_MAIN,
                    Label::new(prev.name_span, ""),
                    "top-level statements cannot coexist with an explicit `fn main`",
                )
                .with_secondary(Label::new(
                    prev.name_span,
                    "explicit `fn main` defined here",
                )),
            );
        } else {
            let id = DefId(defs.len() as u32);
            by_name.insert("main".to_string(), id);
            defs.push(Def {
                id,
                kind: DefKind::SyntheticMain,
                name: "main".to_string(),
                name_span: Span::synthetic(),
                syntax: primary_node,
            });
        }
    }

    // Phase 2 F.3: build each file's effective scope. The scope is:
    //   flat by_name pool + the file's own items + the items
    //   contributed by each `use foo;` declared in that file.
    // Conflicts within a single file's scope diagnose as
    // DUPLICATE_DEFINITION (e.g., `use foo;` brings `add` in but the
    // file already has a local `add`). `use` of an unknown module
    // diagnoses with UNKNOWN_MODULE. Built after the synthetic-main
    // step so the primary file's scope includes `main` when the
    // user wrote top-level statements.
    let mut file_scopes: FxHashMap<FileId, FxHashMap<String, DefId>> = FxHashMap::default();
    for &file in &all_files {
        let mut scope: FxHashMap<String, DefId> = by_name.clone();
        if let Some(items) = file_items.get(&file) {
            for (name, id) in items {
                scope.entry(name.clone()).or_insert(*id);
            }
        }
        if let Some(uses) = file_uses.get(&file) {
            for (used_name, span) in uses {
                let Some(table) = module_tables.get(used_name) else {
                    diags.push(Diagnostic::error(
                        ec::UNKNOWN_MODULE,
                        Label::new(*span, ""),
                        format!("`use {used_name};` references a module not declared by any `liberty {used_name};`"),
                    ));
                    continue;
                };
                let mut entries: Vec<(&String, &DefId)> = table.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                for (name, &id) in entries {
                    if let Some(&prev_id) = scope.get(name) {
                        if prev_id != id {
                            let prev = &defs[prev_id.0 as usize];
                            diags.push(
                                Diagnostic::error(
                                    ec::DUPLICATE_DEFINITION,
                                    Label::new(*span, ""),
                                    format!(
                                        "`use {used_name};` brings `{name}` into scope, but `{name}` is already visible here"
                                    ),
                                )
                                .with_secondary(Label::new(
                                    prev.name_span,
                                    "previous definition",
                                )),
                            );
                        }
                        continue;
                    }
                    scope.insert(name.clone(), id);
                }
            }
        }
        file_scopes.insert(file, scope);
    }

    ResolvedModule {
        module,
        defs,
        by_name,
        file_scopes,
    }
}

/// Walk one module's items, route each fn/class into either the
/// global flat namespace or the module's own table (when the file
/// declares `liberty <name>;`). Collect any `use <name>;` decls into
/// `file_uses` for the F.3 post-pass to resolve into per-file
/// scopes. Phase 2 increments F.2 / F.3.
#[allow(clippy::too_many_arguments)]
fn process_module<'a>(
    module: Module<'a>,
    module_node: &'a SyntaxNode<'a>,
    sm: &SourceMap,
    defs: &mut Vec<Def<'a>>,
    by_name: &mut FxHashMap<String, DefId>,
    module_tables: &mut FxHashMap<String, FxHashMap<String, DefId>>,
    file_items: &mut FxHashMap<FileId, Vec<(String, DefId)>>,
    file_uses: &mut FxHashMap<FileId, Vec<(String, Span)>>,
    all_files: &mut Vec<FileId>,
    is_primary: bool,
    diags: &mut DiagBag,
) {
    let file = module_node.span.file;
    if !all_files.contains(&file) {
        all_files.push(file);
    }
    // Find the file's `liberty <name>;` declaration, if any. Multiple
    // libertys per file diagnose; only the first wins.
    let mut liberty: Option<String> = None;
    for item in module.items() {
        if let Item::Liberty(decl) = item {
            if let Some(name) = liberty_name(decl, sm) {
                if liberty.is_some() {
                    diags.push(Diagnostic::error(
                        ec::DUPLICATE_LIBERTY,
                        Label::new(decl.syntax().span, ""),
                        "file already declared `liberty`; only one per file",
                    ));
                } else {
                    liberty = Some(name);
                }
            }
        }
    }

    // Items go either to the global flat namespace or the file's own
    // module table. We pick the right scope per file up front.
    // Phase 2 F.3: each registered item is also recorded in
    // `file_items[file]` so the post-pass can build per-file scopes
    // that include both the flat pool and the file's own defs.
    for item in module.items() {
        match item {
            Item::Fn(f) => {
                let registered = match liberty.as_deref() {
                    Some(modname) => register_fn(
                        f,
                        sm,
                        defs,
                        module_tables.entry(modname.to_string()).or_default(),
                        diags,
                    ),
                    None => register_fn(f, sm, defs, by_name, diags),
                };
                if let Some(entry) = registered {
                    file_items.entry(file).or_default().push(entry);
                }
            }
            Item::Class(c) => {
                let registered = match liberty.as_deref() {
                    Some(modname) => register_class(
                        c,
                        sm,
                        defs,
                        module_tables.entry(modname.to_string()).or_default(),
                        diags,
                    ),
                    None => register_class(c, sm, defs, by_name, diags),
                };
                if let Some(entry) = registered {
                    file_items.entry(file).or_default().push(entry);
                }
            }
            Item::Use(u) => {
                if let Some(name) = use_name(u, sm) {
                    file_uses
                        .entry(file)
                        .or_default()
                        .push((name, u.syntax().span));
                }
            }
            Item::Liberty(_) | Item::Stub(_) | Item::Error(_) => {
                // Liberty already handled above; Stub/Error already
                // diagnosed by the parser.
            }
        }
    }

    // Top-level statements: only the primary file may have them
    // (synthetic `main`). Sibling files' top-level stmts diagnose,
    // even if the file is liberty-declared (the synthetic-main
    // semantics don't extend to libraries).
    if !is_primary && module.stmts().next().is_some() {
        diags.push(Diagnostic::error(
            ec::TOP_LEVEL_STMTS_IN_LIBRARY,
            Label::new(module_node.span, ""),
            "top-level statements are only allowed in the build target file; sibling `.gw` files must contain only items",
        ));
    }
}

/// Recover the name from a `liberty <name>;` declaration.
fn liberty_name(decl: LibertyDecl<'_>, sm: &SourceMap) -> Option<String> {
    let span = decl.name()?;
    sm.slice(span).map(str::to_string)
}

/// Recover the name from a `use <name>;` declaration.
fn use_name(decl: UseDecl<'_>, sm: &SourceMap) -> Option<String> {
    let span = decl.name()?;
    sm.slice(span).map(str::to_string)
}

fn register_fn<'a>(
    fn_decl: FnDecl<'a>,
    sm: &SourceMap,
    defs: &mut Vec<Def<'a>>,
    by_name: &mut FxHashMap<String, DefId>,
    diags: &mut DiagBag,
) -> Option<(String, DefId)> {
    let Some(name_span) = fn_decl.name() else {
        diags.push(Diagnostic::error(
            ec::MISSING_NAME,
            Label::new(fn_decl.span(), ""),
            "function declaration is missing its name",
        ));
        return None;
    };
    let name = sm.slice(name_span).map(str::to_string).unwrap_or_default();
    let id = DefId(defs.len() as u32);
    if let Some(&prev_id) = by_name.get(&name) {
        let prev = &defs[prev_id.0 as usize];
        diags.push(
            Diagnostic::error(
                ec::DUPLICATE_DEFINITION,
                Label::new(name_span, format!("`{name}` redefined here")),
                format!("the name `{name}` is already defined in this module"),
            )
            .with_secondary(Label::new(
                prev.name_span,
                format!("previous definition of `{name}`"),
            )),
        );
        // Still register so downstream passes can continue.
    } else {
        by_name.insert(name.clone(), id);
    }
    defs.push(Def {
        id,
        kind: DefKind::Fn,
        name: name.clone(),
        name_span,
        syntax: fn_decl.syntax(),
    });
    Some((name, id))
}

fn register_class<'a>(
    class_decl: ClassDecl<'a>,
    sm: &SourceMap,
    defs: &mut Vec<Def<'a>>,
    by_name: &mut FxHashMap<String, DefId>,
    diags: &mut DiagBag,
) -> Option<(String, DefId)> {
    let Some(name_span) = class_decl.name() else {
        diags.push(Diagnostic::error(
            ec::MISSING_NAME,
            Label::new(class_decl.span(), ""),
            "class declaration is missing its name",
        ));
        return None;
    };
    let name = sm.slice(name_span).map(str::to_string).unwrap_or_default();
    let id = DefId(defs.len() as u32);
    if let Some(&prev_id) = by_name.get(&name) {
        let prev = &defs[prev_id.0 as usize];
        diags.push(
            Diagnostic::error(
                ec::DUPLICATE_DEFINITION,
                Label::new(name_span, format!("`{name}` redefined here")),
                format!("the name `{name}` is already defined in this module"),
            )
            .with_secondary(Label::new(
                prev.name_span,
                format!("previous definition of `{name}`"),
            )),
        );
    } else {
        by_name.insert(name.clone(), id);
    }
    defs.push(Def {
        id,
        kind: DefKind::Class,
        name: name.clone(),
        name_span,
        syntax: class_decl.syntax(),
    });
    Some((name, id))
}

/// Built-in primitive type names. Returns `Some` if the identifier
/// matches a primitive recognised by the Phase-1 type checker. The
/// resolver itself does not need this — the type checker uses it — but
/// it lives here so name-binding decisions are centralised.
pub fn primitive_type_name(name: &str) -> Option<PrimitiveTy> {
    Some(match name {
        "u0" => PrimitiveTy::U0,
        "bool" => PrimitiveTy::Bool,
        "i8" => PrimitiveTy::I8,
        "i16" => PrimitiveTy::I16,
        "i32" => PrimitiveTy::I32,
        "i64" => PrimitiveTy::I64,
        "u8" => PrimitiveTy::U8,
        "u16" => PrimitiveTy::U16,
        "u32" => PrimitiveTy::U32,
        "u64" => PrimitiveTy::U64,
        "isize" => PrimitiveTy::ISize,
        "usize" => PrimitiveTy::USize,
        "f32" => PrimitiveTy::F32,
        "f64" => PrimitiveTy::F64,
        "rune" => PrimitiveTy::Rune,
        _ => return None,
    })
}

/// Built-in primitive types per `docs/spec.md` §5.4.1. Re-exported from
/// the resolver because primitive resolution is part of name binding;
/// the type checker consumes this enum.
#[allow(missing_docs)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PrimitiveTy {
    U0,
    Bool,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    ISize,
    USize,
    F32,
    F64,
    Rune,
}

#[cfg(test)]
mod tests {
    use super::*;
    use arsenal_ast::FileArena;
    use arsenal_lex::SourceMap;
    use arsenal_parse::parse;
    use bumpalo::Bump;

    fn run_resolver(src: &str) -> (Vec<String>, u32) {
        let mut sm = SourceMap::new();
        let file = sm.add_file("t.gw", src);
        let bytes = sm.get(file).unwrap().contents.as_bytes();
        let bump = Bump::new();
        let arena = FileArena::new(&bump, file);
        let (root, mut diags) = parse(file, bytes, &arena);
        let resolved = resolve_module(root, &sm, &mut diags);
        let names: Vec<_> = resolved.defs.iter().map(|d| d.name.clone()).collect();
        (names, diags.error_count())
    }

    #[test]
    fn registers_top_level_fns() {
        let (names, _) = run_resolver("fn a() -> u0 {} fn b() -> u0 {}");
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn duplicate_def_emits_error_and_continues() {
        let (names, errs) = run_resolver("fn a() -> u0 {} fn a() -> u0 {}");
        assert_eq!(names, vec!["a", "a"]);
        assert_eq!(errs, 1);
    }

    #[test]
    fn primitive_lookup() {
        assert_eq!(primitive_type_name("i32"), Some(PrimitiveTy::I32));
        assert_eq!(primitive_type_name("bool"), Some(PrimitiveTy::Bool));
        assert_eq!(primitive_type_name("u0"), Some(PrimitiveTy::U0));
        assert_eq!(primitive_type_name("Vec3"), None);
    }

    fn run_resolver_full(src: &str) -> (Vec<(String, DefKind)>, u32) {
        let mut sm = SourceMap::new();
        let file = sm.add_file("t.gw", src);
        let bytes = sm.get(file).unwrap().contents.as_bytes();
        let bump = Bump::new();
        let arena = FileArena::new(&bump, file);
        let (root, mut diags) = parse(file, bytes, &arena);
        let resolved = resolve_module(root, &sm, &mut diags);
        let entries: Vec<_> = resolved
            .defs
            .iter()
            .map(|d| (d.name.clone(), d.kind))
            .collect();
        (entries, diags.error_count())
    }

    #[test]
    fn top_level_stmts_synthesise_main() {
        let (entries, errs) = run_resolver_full("let x: i32 = 1; return x;");
        assert_eq!(errs, 0);
        assert_eq!(entries, vec![("main".to_string(), DefKind::SyntheticMain)]);
    }

    #[test]
    fn items_only_no_synthetic_main() {
        let (entries, errs) = run_resolver_full("fn main() -> i32 { return 0; }");
        assert_eq!(errs, 0);
        assert_eq!(entries, vec![("main".to_string(), DefKind::Fn)]);
    }

    #[test]
    fn top_level_stmts_with_explicit_main_errors() {
        let (entries, errs) = run_resolver_full("fn main() -> i32 { return 0; } let x: i32 = 1;");
        assert_eq!(errs, 1);
        // The explicit `fn main` is registered; the synthetic is skipped.
        assert_eq!(entries, vec![("main".to_string(), DefKind::Fn)]);
    }

    #[test]
    fn top_level_stmts_alongside_other_items() {
        let (entries, errs) = run_resolver_full(
            "extern fn putchar(c: i32) -> i32; class Foo { x: i32 } let x: i32 = 1; return x;",
        );
        assert_eq!(errs, 0);
        assert_eq!(
            entries,
            vec![
                ("putchar".to_string(), DefKind::Fn),
                ("Foo".to_string(), DefKind::Class),
                ("main".to_string(), DefKind::SyntheticMain),
            ]
        );
    }

    // ─── Phase 2 increment F.1: cross-file resolve ─────────────────────────

    fn run_multi_resolver(primary: &str, extras: &[&str]) -> (Vec<String>, u32) {
        let mut sm = SourceMap::new();
        let bump = Bump::new();

        let primary_file = sm.add_file("primary.gw", primary);
        let primary_bytes = sm.get(primary_file).unwrap().contents.as_bytes();
        let primary_arena = FileArena::new(&bump, primary_file);
        let (primary_root, mut diags) = parse(primary_file, primary_bytes, &primary_arena);

        let mut extra_roots = Vec::with_capacity(extras.len());
        for (i, src) in extras.iter().enumerate() {
            let f = sm.add_file(format!("extra{i}.gw"), *src);
            let bytes = sm.get(f).unwrap().contents.as_bytes();
            let arena = FileArena::new(&bump, f);
            let (root, sib_diags) = parse(f, bytes, &arena);
            diags.merge(sib_diags);
            extra_roots.push(root);
        }

        let resolved = resolve_modules(primary_root, &extra_roots, &sm, &mut diags);
        let names: Vec<_> = resolved.defs.iter().map(|d| d.name.clone()).collect();
        (names, diags.error_count())
    }

    #[test]
    fn multi_file_register_fns_from_each_module() {
        // `add` lives in the extra module; `main` in the primary.
        let (names, errs) = run_multi_resolver(
            "fn main() -> i32 { return add(2, 3); }",
            &["fn add(a: i32, b: i32) -> i32 { return a + b; }"],
        );
        assert_eq!(errs, 0);
        // Primary's `main` registers first, then the extra's `add`.
        assert_eq!(names, vec!["main", "add"]);
    }

    #[test]
    fn multi_file_duplicate_across_files_diagnoses() {
        let (_, errs) = run_multi_resolver(
            "fn add() -> i32 { return 1; } fn main() -> i32 { return add(); }",
            &["fn add() -> i32 { return 2; }"],
        );
        assert!(errs >= 1);
    }

    #[test]
    fn multi_file_top_level_stmts_in_extra_diagnose() {
        // Sibling files must contain only items; top-level stmts in
        // a sibling diagnose with TOP_LEVEL_STMTS_IN_LIBRARY.
        let (_, errs) =
            run_multi_resolver("fn main() -> i32 { return 0; }", &["let global: i32 = 7;"]);
        assert!(errs >= 1);
    }

    // ─── Phase 2 increment F.2: liberty + use ─────────────────────────────

    #[test]
    fn liberty_items_invisible_without_use() {
        // `add` lives in a `liberty math;` file; main doesn't `use
        // math;`. The resolver itself stays diag-free (typeck flags
        // the missing call name); but `add` is in the module table,
        // not the flat namespace.
        let (names, errs) = run_multi_resolver(
            "fn main() -> i32 { return add(2, 3); }",
            &["liberty math; fn add(a: i32, b: i32) -> i32 { return a + b; }"],
        );
        assert!(names.contains(&"main".to_string()));
        assert!(names.contains(&"add".to_string()));
        assert_eq!(errs, 0);
    }

    #[test]
    fn use_brings_liberty_items_into_scope() {
        let (_, errs) = run_multi_resolver(
            "use math; fn main() -> i32 { return add(2, 3); }",
            &["liberty math; fn add(a: i32, b: i32) -> i32 { return a + b; }"],
        );
        assert_eq!(errs, 0);
    }

    #[test]
    fn use_of_unknown_module_diagnoses() {
        let (_, errs) = run_multi_resolver("use missing; fn main() -> i32 { return 0; }", &[]);
        assert!(errs >= 1);
    }

    #[test]
    fn duplicate_liberty_in_same_file_diagnoses() {
        let (_, errs) = run_multi_resolver(
            "fn main() -> i32 { return 0; }",
            &["liberty math; liberty algebra; fn add() -> i32 { return 1; }"],
        );
        assert!(errs >= 1);
    }

    // ─── Phase 2 increment F.3: per-file `use` scoping ───────────────────

    #[test]
    fn use_only_visible_in_declaring_file() {
        // main.gw `use math;`, lib.gw doesn't. Both files have items
        // (lib.gw is flat-namespace, no liberty). lib.gw's `helper`
        // refers to `add`, which lives in `liberty math;` and only
        // main.gw imported it. lib.gw's scope shouldn't include
        // `add` — typeck (not the resolver) flags the unknown
        // reference, but the resolver's per-file scope construction
        // is what enables that.
        let mut sm = SourceMap::new();
        let bump = Bump::new();

        let main_file = sm.add_file("main.gw", "use math; fn main() -> i32 { return 0; }");
        let main_bytes = sm.get(main_file).unwrap().contents.as_bytes();
        let main_arena = FileArena::new(&bump, main_file);
        let (main_root, mut diags) = parse(main_file, main_bytes, &main_arena);

        let lib_file = sm.add_file("lib.gw", "fn helper() -> i32 { return 0; }");
        let lib_bytes = sm.get(lib_file).unwrap().contents.as_bytes();
        let lib_arena = FileArena::new(&bump, lib_file);
        let (lib_root, lib_diags) = parse(lib_file, lib_bytes, &lib_arena);
        diags.merge(lib_diags);

        let math_file = sm.add_file(
            "math.gw",
            "liberty math; fn add(a: i32, b: i32) -> i32 { return a + b; }",
        );
        let math_bytes = sm.get(math_file).unwrap().contents.as_bytes();
        let math_arena = FileArena::new(&bump, math_file);
        let (math_root, math_diags) = parse(math_file, math_bytes, &math_arena);
        diags.merge(math_diags);

        let resolved = resolve_modules(main_root, &[lib_root, math_root], &sm, &mut diags);
        assert_eq!(diags.error_count(), 0);

        // main.gw's scope should include `add` (via `use math;`).
        assert!(resolved.lookup_in_file(main_file, "add").is_some());
        // lib.gw's scope should NOT include `add` — it didn't `use math;`.
        assert!(resolved.lookup_in_file(lib_file, "add").is_none());
        // Both files should see the flat-namespace items (`helper`).
        assert!(resolved.lookup_in_file(main_file, "helper").is_some());
        assert!(resolved.lookup_in_file(lib_file, "helper").is_some());
        // Both files should see the synthetic `main` if it was
        // synthesised — here it's an explicit fn, not synthetic.
        assert!(resolved.lookup_in_file(main_file, "main").is_some());
    }

    #[test]
    fn use_then_collide_with_local_diagnoses() {
        // Both files define `add`: primary in flat namespace,
        // sibling under `liberty math;`. `use math;` brings the
        // sibling's `add` into the flat namespace and collides with
        // the local one.
        let (_, errs) = run_multi_resolver(
            "use math;\n\
             fn add() -> i32 { return 99; }\n\
             fn main() -> i32 { return add(); }",
            &["liberty math; fn add() -> i32 { return 0; }"],
        );
        assert!(errs >= 1);
    }
}
