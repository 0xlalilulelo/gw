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

use arsenal_ast::{AstNode, ClassDecl, FnDecl, Item, Module, SyntaxNode};
use arsenal_lex::{DiagBag, Diagnostic, Label, SourceMap, Span};
use rustc_hash::FxHashMap;

/// Resolver error codes. Reserved range: `E0200..E0299`.
pub mod ec {
    use arsenal_lex::ErrorCode;
    /// A name is declared more than once at the same scope.
    pub const DUPLICATE_DEFINITION: ErrorCode = ErrorCode(200);
    /// A function declaration has no name token.
    pub const MISSING_NAME: ErrorCode = ErrorCode(201);
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
    /// Lookup from source name to [`DefId`].
    pub by_name: FxHashMap<String, DefId>,
}

impl<'a> ResolvedModule<'a> {
    /// Locate a definition by source name.
    pub fn lookup(&self, name: &str) -> Option<&Def<'a>> {
        self.by_name.get(name).map(|id| &self.defs[id.0 as usize])
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
    let module =
        Module::cast(module_node).expect("resolve_module called on non-Module syntax node");
    let mut defs: Vec<Def<'a>> = Vec::new();
    let mut by_name: FxHashMap<String, DefId> = FxHashMap::default();

    for item in module.items() {
        match item {
            Item::Fn(f) => register_fn(f, sm, &mut defs, &mut by_name, diags),
            Item::Class(c) => register_class(c, sm, &mut defs, &mut by_name, diags),
            _ => {
                // Phase 0 parser already produced diagnostics for items
                // it couldn't classify; the resolver doesn't add more.
            }
        }
    }

    ResolvedModule {
        module,
        defs,
        by_name,
    }
}

fn register_fn<'a>(
    fn_decl: FnDecl<'a>,
    sm: &SourceMap,
    defs: &mut Vec<Def<'a>>,
    by_name: &mut FxHashMap<String, DefId>,
    diags: &mut DiagBag,
) {
    let Some(name_span) = fn_decl.name() else {
        diags.push(Diagnostic::error(
            ec::MISSING_NAME,
            Label::new(fn_decl.span(), ""),
            "function declaration is missing its name",
        ));
        return;
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
        name,
        name_span,
        syntax: fn_decl.syntax(),
    });
}

fn register_class<'a>(
    class_decl: ClassDecl<'a>,
    sm: &SourceMap,
    defs: &mut Vec<Def<'a>>,
    by_name: &mut FxHashMap<String, DefId>,
    diags: &mut DiagBag,
) {
    let Some(name_span) = class_decl.name() else {
        diags.push(Diagnostic::error(
            ec::MISSING_NAME,
            Label::new(class_decl.span(), ""),
            "class declaration is missing its name",
        ));
        return;
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
        name,
        name_span,
        syntax: class_decl.syntax(),
    });
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
}
