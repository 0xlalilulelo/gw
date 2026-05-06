//! GW type checker — Phase 1 minimum.
//!
//! See `docs/architecture.md` Part B.6 and Part D.1. The Phase-1 slice
//! supports:
//!
//! - Primitive types only: `i8..i64`, `u8..u64`, `isize`, `usize`,
//!   `bool`, `u0`, `f32`, `f64`, `rune`.
//! - Bidirectional inference for the operators recognised by the
//!   parser (binary arithmetic, comparison, logical, bitwise, shift;
//!   unary `-`, `!`, `~`).
//! - `let x: T = expr;` checks `expr` against `T`.
//! - `return expr` checks `expr` against the enclosing fn's return
//!   type.
//! - Calls: argument count + per-arg checking against the callee's
//!   declared parameter types.
//!
//! Out of scope (Phase 2+): generics, ciphers, classes, error
//! unions, comptime, type inference for nested literals.
//!
//! Output: a [`TypedModule`] mapping each [`DefId`] to a typed function
//! signature plus a per-expression "what type" map keyed by CST node
//! pointer.
//!
//! Errors are pushed into the caller's [`DiagBag`]; the type checker
//! always returns a `TypedModule` even if checking failed, so MIR
//! lowering can still produce best-effort output.

use arsenal_ast::{
    AstNode, BinaryExpr, Block, BreakExpr, CallExpr, ClassDecl, ContinueExpr, Expr, ExprStmt,
    FieldExpr, FnDecl, ForExpr, IfExpr, LetStmt, LiteralExpr, Module, ParenExpr, PathExpr,
    PathType, Pattern, ReturnExpr, Stmt, StructLitExpr, SyntaxKind, SyntaxNode, Type, UnaryExpr,
    WhileExpr,
};
use arsenal_lex::{DiagBag, Diagnostic, Label, SourceMap, Span};
use arsenal_resolve::{primitive_type_name, DefId, DefKind, ResolvedModule};
use rustc_hash::FxHashMap;

/// Type-checker error codes. Reserved range: `E0300..E0399`.
pub mod ec {
    use arsenal_lex::ErrorCode;
    /// Mismatch between expected and actual type.
    pub const TYPE_MISMATCH: ErrorCode = ErrorCode(300);
    /// Reference to an unknown type name.
    pub const UNKNOWN_TYPE: ErrorCode = ErrorCode(301);
    /// Reference to an unknown name in an expression.
    pub const UNKNOWN_NAME: ErrorCode = ErrorCode(302);
    /// Wrong number of arguments to a function call.
    pub const WRONG_ARG_COUNT: ErrorCode = ErrorCode(303);
    /// Operator applied to operand of an unsupported type.
    pub const BAD_OPERAND: ErrorCode = ErrorCode(304);
    /// A `return` expression had no value but the enclosing function
    /// requires one (or vice-versa).
    pub const RETURN_VALUE_MISMATCH: ErrorCode = ErrorCode(305);
    /// A construct outside the Phase-1 supported subset was reached.
    pub const UNSUPPORTED_CONSTRUCT: ErrorCode = ErrorCode(306);
    /// Function declaration is missing its return type annotation
    /// (Phase 1 requires explicit return types).
    pub const MISSING_RETURN_TYPE: ErrorCode = ErrorCode(307);
    /// Function parameter is missing its type annotation.
    pub const MISSING_PARAM_TYPE: ErrorCode = ErrorCode(308);
    /// `break` or `continue` used outside of a loop body.
    pub const BREAK_OUTSIDE_LOOP: ErrorCode = ErrorCode(309);
    /// Struct literal has the wrong field set, or `obj.x` references a
    /// field the class doesn't declare.
    pub const UNKNOWN_FIELD: ErrorCode = ErrorCode(310);
    /// Struct literal initialises the same field more than once or
    /// omits a required field.
    pub const FIELD_INIT_MISMATCH: ErrorCode = ErrorCode(311);
    /// Field access on a non-class value.
    pub const FIELD_ON_NON_CLASS: ErrorCode = ErrorCode(312);
    /// Struct-literal path didn't resolve to a class.
    pub const NOT_A_CLASS: ErrorCode = ErrorCode(313);
}

/// Concrete type. Phase-1 supports primitives and POD classes;
/// generics extend this in later phases.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum Ty {
    /// `u0` — zero-sized unit (spec §5.4.1).
    U0,
    /// `bool`.
    Bool,
    /// Integer primitive.
    Int(IntTy),
    /// Floating-point primitive.
    Float(FloatTy),
    /// `rune` — UTF-8 scalar.
    Rune,
    /// User-defined POD class identified by its [`DefId`]. Phase 1
    /// disallows classes in function signatures (a "Phase 2" deferred
    /// limitation, since cross-fn passing requires by-pointer ABI work).
    Class(DefId),
    /// `[]T` — fat pointer (data + length) over an element type. Phase 1
    /// only accepts `[]u8` for string literals; other element types and
    /// cross-fn slice passing are deferred.
    Slice(IntTy),
    /// `*T` — raw pointer (spec §5.4). Phase 1 accepts only `*u8` /
    /// `*i8` and only in extern fn signatures and as the type of
    /// `slice.data`. Cross-fn pointer passing in non-extern fns is
    /// deferred (memory model + borrow-checker work).
    Ptr(IntTy),
    /// Synthetic placeholder when type checking failed for an
    /// expression. Treated as compatible with any expected type so a
    /// single failure does not cascade.
    Error,
}

/// Integer primitives.
#[allow(missing_docs)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    ISize,
    U8,
    U16,
    U32,
    U64,
    USize,
}

/// Floating-point primitives.
#[allow(missing_docs)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum FloatTy {
    F32,
    F64,
}

impl Ty {
    /// Whether this type permits integer arithmetic / bitwise / shift
    /// operators.
    pub fn is_integer(self) -> bool {
        matches!(self, Self::Int(_))
    }

    /// Whether this type permits floating-point arithmetic.
    pub fn is_float(self) -> bool {
        matches!(self, Self::Float(_))
    }

    /// Whether this type is one of the numeric kinds (integer or
    /// floating-point).
    pub fn is_numeric(self) -> bool {
        self.is_integer() || self.is_float()
    }

    /// Bit width of an integer type, in bits, given the host's pointer
    /// width for `isize`/`usize`. Returns `None` for non-integer types.
    pub fn int_bits(self, ptr_bits: u32) -> Option<u32> {
        Some(match self {
            Self::Int(IntTy::I8) | Self::Int(IntTy::U8) => 8,
            Self::Int(IntTy::I16) | Self::Int(IntTy::U16) => 16,
            Self::Int(IntTy::I32) | Self::Int(IntTy::U32) => 32,
            Self::Int(IntTy::I64) | Self::Int(IntTy::U64) => 64,
            Self::Int(IntTy::ISize) | Self::Int(IntTy::USize) => ptr_bits,
            _ => return None,
        })
    }

    /// Whether this is a signed integer type. Returns `false` for
    /// unsigned and non-integer types.
    pub fn is_signed_int(self) -> bool {
        matches!(
            self,
            Self::Int(IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::ISize)
        )
    }
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::U0 => f.write_str("u0"),
            Self::Bool => f.write_str("bool"),
            Self::Int(t) => f.write_str(match t {
                IntTy::I8 => "i8",
                IntTy::I16 => "i16",
                IntTy::I32 => "i32",
                IntTy::I64 => "i64",
                IntTy::ISize => "isize",
                IntTy::U8 => "u8",
                IntTy::U16 => "u16",
                IntTy::U32 => "u32",
                IntTy::U64 => "u64",
                IntTy::USize => "usize",
            }),
            Self::Float(t) => f.write_str(match t {
                FloatTy::F32 => "f32",
                FloatTy::F64 => "f64",
            }),
            Self::Rune => f.write_str("rune"),
            Self::Class(def_id) => write!(f, "<class#{}>", def_id.0),
            Self::Slice(elem) => write!(f, "[]{}", Self::Int(*elem)),
            Self::Ptr(elem) => write!(f, "*{}", Self::Int(*elem)),
            Self::Error => f.write_str("<error>"),
        }
    }
}

/// Typed signature of a function definition.
#[derive(Clone, Debug)]
pub struct FnSig {
    /// Parameter types, in declaration order.
    pub params: Vec<Param>,
    /// Return type. Phase 1 requires an explicit annotation; missing
    /// annotations are reported and the field defaults to [`Ty::Error`].
    pub ret: Ty,
}

/// One parameter in a [`FnSig`].
#[derive(Clone, Debug)]
pub struct Param {
    /// Source name of the parameter.
    pub name: String,
    /// Source span of the parameter's name.
    pub name_span: Span,
    /// Resolved parameter type.
    pub ty: Ty,
}

/// Resolved layout of one user-declared class.
#[derive(Clone, Debug)]
pub struct ClassLayout {
    /// Source name (mostly for diagnostics).
    pub name: String,
    /// Fields in declaration order. Their types are already resolved;
    /// codegen consumes this to compute byte offsets.
    pub fields: Vec<ClassField>,
}

/// One field within a [`ClassLayout`].
#[derive(Clone, Debug)]
pub struct ClassField {
    /// Source name of the field.
    pub name: String,
    /// Source span of the field's name (for diagnostics).
    pub name_span: Span,
    /// Resolved field type.
    pub ty: Ty,
}

/// Result of type-checking a [`ResolvedModule`].
pub struct TypedModule<'a> {
    /// Borrowed resolved module (so MIR lowering can still walk the
    /// CST).
    pub resolved: &'a ResolvedModule<'a>,
    /// Per-definition signature, indexed by [`DefId`]. Only populated
    /// for `DefKind::Fn` definitions.
    pub sigs: FxHashMap<DefId, FnSig>,
    /// Per-definition class layout, indexed by [`DefId`]. Only
    /// populated for `DefKind::Class` definitions.
    pub classes: FxHashMap<DefId, ClassLayout>,
    /// Per-CST-node expression type (keyed by node pointer identity).
    pub expr_types: FxHashMap<NodePtr<'a>, Ty>,
    /// Resolved callee for each [`SyntaxKind::CallExpr`] node.
    pub call_targets: FxHashMap<NodePtr<'a>, DefId>,
    /// Per-CST-node binding from [`SyntaxKind::PathExpr`] to a parameter
    /// or `let`-bound local.
    pub path_bindings: FxHashMap<NodePtr<'a>, BindingId>,
    /// Per-CST-node binding from a *binding* node — `Param` or
    /// `IdentPat` — to the [`BindingId`] allocated for it. MIR
    /// lowering uses this to keep its `Local` allocation order in
    /// sync with typeck's `BindingId` allocation order, which differ
    /// because MIR introduces fresh `Local`s for expression
    /// intermediates that typeck does not.
    pub pat_bindings: FxHashMap<NodePtr<'a>, BindingId>,
}

/// Pointer-identity key into the bump-allocated CST.
///
/// Equality and hashing are by *address* of the underlying
/// [`SyntaxNode`], not by structural value — distinct CST nodes with
/// identical content compare unequal. The bump arena guarantees stable
/// addresses for the lifetime `'a`.
#[derive(Copy, Clone)]
pub struct NodePtr<'a>(pub &'a SyntaxNode<'a>);

impl<'a> PartialEq for NodePtr<'a> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}
impl<'a> Eq for NodePtr<'a> {}

impl<'a> std::hash::Hash for NodePtr<'a> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.0 as *const SyntaxNode<'a>).hash(state);
    }
}

impl<'a> std::fmt::Debug for NodePtr<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NodePtr({:p})", self.0)
    }
}

/// Identifies a parameter or local within a function. Indices are
/// assigned in source order: parameters first, then `let`-bound locals
/// in the order they appear in the body.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BindingId(pub u32);

/// Type-check `resolved`. Errors push into `diags`. The returned
/// [`TypedModule`] always has an entry per definition; entries for
/// failed checks contain `Ty::Error` placeholders so downstream passes
/// can continue.
pub fn type_check<'a>(
    resolved: &'a ResolvedModule<'a>,
    sm: &SourceMap,
    diags: &mut DiagBag,
) -> TypedModule<'a> {
    // Pass 1a: collect class layouts. Classes go before fn signatures
    // so that fn signatures referring to a class type resolve cleanly.
    // (Phase 1 currently rejects classes in fn signatures; the ordering
    // still matters for diagnostics.)
    let mut classes: FxHashMap<DefId, ClassLayout> = FxHashMap::default();
    for def in &resolved.defs {
        if def.kind != DefKind::Class {
            continue;
        }
        let class_decl = ClassDecl::cast(def.syntax)
            .expect("resolver registered DefKind::Class with non-ClassDecl syntax");
        let layout = check_class_layout(def.name.clone(), class_decl, resolved, sm, diags);
        classes.insert(def.id, layout);
    }

    // Pass 1b: collect fn signatures so calls can reference forward.
    let mut sigs: FxHashMap<DefId, FnSig> = FxHashMap::default();
    for def in &resolved.defs {
        match def.kind {
            DefKind::Fn => {
                let fn_decl = FnDecl::cast(def.syntax).expect("DefKind::Fn syntax must be FnDecl");
                let sig = check_fn_signature(fn_decl, resolved, sm, diags);
                sigs.insert(def.id, sig);
            }
            DefKind::SyntheticMain => {
                // Synthetic `main` from top-level statements: no params,
                // returns i32 (POSIX exit code). Phase 1 increment 11a.
                sigs.insert(
                    def.id,
                    FnSig {
                        params: Vec::new(),
                        ret: Ty::Int(IntTy::I32),
                    },
                );
            }
            DefKind::Class => {}
        }
    }

    // Pass 2: check function bodies.
    let mut tm = TypedModule {
        resolved,
        sigs,
        classes,
        expr_types: FxHashMap::default(),
        call_targets: FxHashMap::default(),
        path_bindings: FxHashMap::default(),
        pat_bindings: FxHashMap::default(),
    };
    for def in &resolved.defs {
        match def.kind {
            DefKind::Fn => {
                let fn_decl = FnDecl::cast(def.syntax).unwrap();
                check_fn_body(def.id, fn_decl, sm, &mut tm, diags);
            }
            DefKind::SyntheticMain => {
                let module =
                    Module::cast(def.syntax).expect("DefKind::SyntheticMain syntax must be Module");
                check_synthetic_main_body(module, sm, &mut tm, diags);
            }
            DefKind::Class => {}
        }
    }
    tm
}

fn check_class_layout(
    name: String,
    class_decl: ClassDecl<'_>,
    resolved: &ResolvedModule<'_>,
    sm: &SourceMap,
    diags: &mut DiagBag,
) -> ClassLayout {
    let mut fields = Vec::new();
    if let Some(list) = class_decl.fields() {
        for fd in list.fields() {
            let name_span = fd.name();
            let f_name = name_span
                .and_then(|s| sm.slice(s).map(str::to_string))
                .unwrap_or_default();
            let ty = match fd.ty() {
                Some(t) => resolve_type(t, resolved, sm, diags),
                None => {
                    diags.push(Diagnostic::error(
                        ec::MISSING_PARAM_TYPE,
                        Label::new(fd.span(), ""),
                        format!(
                            "field `{}` is missing its type annotation",
                            if f_name.is_empty() { "_" } else { &f_name },
                        ),
                    ));
                    Ty::Error
                }
            };
            // Phase 1 disallows nested class fields to keep codegen simple.
            // (Nested classes would require recursive size/offset
            // computation that we punt to Phase 2.) Slice fields are
            // similarly deferred — class layout would need to embed the
            // slice's `(data, len)` pair.
            if matches!(ty, Ty::Class(_)) {
                diags.push(Diagnostic::error(
                    ec::UNSUPPORTED_CONSTRUCT,
                    Label::new(fd.span(), ""),
                    "nested class fields are not yet supported in Phase 1",
                ));
            }
            if matches!(ty, Ty::Slice(_)) {
                diags.push(Diagnostic::error(
                    ec::UNSUPPORTED_CONSTRUCT,
                    Label::new(fd.span(), ""),
                    "slice-typed class fields are not yet supported in Phase 1",
                ));
            }
            fields.push(ClassField {
                name: f_name,
                name_span: name_span.unwrap_or_else(Span::synthetic),
                ty,
            });
        }
    }
    ClassLayout { name, fields }
}

fn check_fn_signature(
    fn_decl: FnDecl<'_>,
    resolved: &ResolvedModule<'_>,
    sm: &SourceMap,
    diags: &mut DiagBag,
) -> FnSig {
    let is_extern = fn_decl.is_extern();
    let mut params = Vec::new();
    if let Some(plist) = fn_decl.params() {
        for p in plist.params() {
            let name_span = p.name();
            let name = name_span
                .and_then(|s| sm.slice(s).map(str::to_string))
                .unwrap_or_default();
            let ty = match p.ty() {
                Some(t) => resolve_type(t, resolved, sm, diags),
                None => {
                    diags.push(Diagnostic::error(
                        ec::MISSING_PARAM_TYPE,
                        Label::new(p.span(), ""),
                        format!(
                            "parameter `{name}` is missing its type annotation",
                            name = if name.is_empty() { "_" } else { &name },
                        ),
                    ));
                    Ty::Error
                }
            };
            // Phase 1 disallows class- and slice-typed params: cross-fn
            // passing requires a by-pointer ABI we haven't built yet.
            if matches!(ty, Ty::Class(_)) {
                diags.push(Diagnostic::error(
                    ec::UNSUPPORTED_CONSTRUCT,
                    Label::new(p.span(), ""),
                    "class-typed parameters are not yet supported in Phase 1; pass primitive fields instead",
                ));
            }
            if matches!(ty, Ty::Slice(_)) {
                diags.push(Diagnostic::error(
                    ec::UNSUPPORTED_CONSTRUCT,
                    Label::new(p.span(), ""),
                    "slice-typed parameters are not yet supported in Phase 1",
                ));
            }
            // Raw pointers (`*T`) are only allowed at the FFI boundary —
            // i.e. `extern fn` declarations. Cross-fn pointer flow inside
            // user code is deferred to a later increment along with the
            // memory model.
            if matches!(ty, Ty::Ptr(_)) && !is_extern {
                diags.push(Diagnostic::error(
                    ec::UNSUPPORTED_CONSTRUCT,
                    Label::new(p.span(), ""),
                    "raw pointer parameters are only allowed in `extern fn` declarations in Phase 1",
                ));
            }
            params.push(Param {
                name,
                name_span: name_span.unwrap_or_else(Span::synthetic),
                ty,
            });
        }
    }
    let ret = match fn_decl.ret_type().and_then(|rt| rt.ty()) {
        Some(t) => resolve_type(t, resolved, sm, diags),
        None => {
            diags.push(Diagnostic::error(
                ec::MISSING_RETURN_TYPE,
                Label::new(fn_decl.span(), ""),
                "function declaration is missing its return type; Phase 1 requires explicit `-> T`",
            ));
            Ty::Error
        }
    };
    if matches!(ret, Ty::Class(_)) {
        diags.push(Diagnostic::error(
            ec::UNSUPPORTED_CONSTRUCT,
            Label::new(fn_decl.span(), ""),
            "class-typed return values are not yet supported in Phase 1",
        ));
    }
    if matches!(ret, Ty::Ptr(_)) && !is_extern {
        diags.push(Diagnostic::error(
            ec::UNSUPPORTED_CONSTRUCT,
            Label::new(fn_decl.span(), ""),
            "raw pointer return types are only allowed in `extern fn` declarations in Phase 1",
        ));
    }
    if matches!(ret, Ty::Slice(_)) {
        diags.push(Diagnostic::error(
            ec::UNSUPPORTED_CONSTRUCT,
            Label::new(fn_decl.span(), ""),
            "slice-typed return values are not yet supported in Phase 1",
        ));
    }
    FnSig { params, ret }
}

fn resolve_type(
    ty: Type<'_>,
    resolved: &ResolvedModule<'_>,
    sm: &SourceMap,
    diags: &mut DiagBag,
) -> Ty {
    let path = match ty {
        Type::Path(p) => p,
        Type::Slice(s) => {
            // Phase 1 only supports `[]u8`; everything else is rejected
            // until cross-fn slice passing and generic element types land.
            let elem_ty = s
                .element()
                .map(|e| resolve_type(e, resolved, sm, diags))
                .unwrap_or(Ty::Error);
            return match elem_ty {
                Ty::Int(IntTy::U8) => Ty::Slice(IntTy::U8),
                Ty::Error => Ty::Error,
                other => {
                    diags.push(Diagnostic::error(
                        ec::UNSUPPORTED_CONSTRUCT,
                        Label::new(s.syntax().span, ""),
                        format!(
                            "Phase 1 only supports `[]u8` slices; element type `{other}` is not yet supported"
                        ),
                    ));
                    Ty::Error
                }
            };
        }
        Type::Ptr(p) => {
            // Phase 1 only supports `*u8` / `*i8`; richer pointee types
            // require the memory model that lands with the borrow checker.
            let elem_ty = p
                .element()
                .map(|e| resolve_type(e, resolved, sm, diags))
                .unwrap_or(Ty::Error);
            return match elem_ty {
                Ty::Int(IntTy::U8) => Ty::Ptr(IntTy::U8),
                Ty::Int(IntTy::I8) => Ty::Ptr(IntTy::I8),
                Ty::Error => Ty::Error,
                other => {
                    diags.push(Diagnostic::error(
                        ec::UNSUPPORTED_CONSTRUCT,
                        Label::new(p.syntax().span, ""),
                        format!(
                            "Phase 1 only supports `*u8` / `*i8` raw pointers; pointee `{other}` is not yet supported"
                        ),
                    ));
                    Ty::Error
                }
            };
        }
        _ => {
            diags.push(Diagnostic::error(
                ec::UNSUPPORTED_CONSTRUCT,
                Label::new(ty_span(&ty), ""),
                "Phase 1 supports only path-typed primitives, classes, `[]u8` slices, and `*u8` / `*i8` raw pointers",
            ));
            return Ty::Error;
        }
    };
    if let Some(prim) = primitive_from_path(path, sm) {
        return prim;
    }
    // Try class lookup.
    if let Some(name) = single_segment_name(path, sm) {
        if let Some(def) = resolved.lookup(name) {
            if def.kind == arsenal_resolve::DefKind::Class {
                return Ty::Class(def.id);
            }
        }
    }
    let span = path.syntax().span;
    let name = path
        .segments()
        .filter_map(|s| sm.slice(s))
        .collect::<Vec<_>>()
        .join("::");
    diags.push(Diagnostic::error(
        ec::UNKNOWN_TYPE,
        Label::new(span, ""),
        format!("unknown type `{name}`"),
    ));
    Ty::Error
}

fn single_segment_name<'a>(path: PathType<'a>, sm: &'a SourceMap) -> Option<&'a str> {
    let mut iter = path.segments();
    let first = iter.next()?;
    if iter.next().is_some() {
        return None;
    }
    sm.slice(first)
}

fn ty_span(t: &Type<'_>) -> Span {
    match t {
        Type::Path(p) => p.syntax().span,
        Type::Ref(p) => p.syntax().span,
        Type::Opt(p) => p.syntax().span,
        Type::Slice(p) => p.syntax().span,
        Type::Array(p) => p.syntax().span,
        Type::Ptr(p) => p.syntax().span,
        Type::Stub(n) | Type::Error(n) => n.span,
    }
}

fn primitive_from_path(path: PathType<'_>, sm: &SourceMap) -> Option<Ty> {
    let mut iter = path.segments();
    let first = iter.next()?;
    if iter.next().is_some() {
        // Multi-segment paths can't be primitives.
        return None;
    }
    let name = sm.slice(first)?;
    primitive_type_name(name).map(|p| match p {
        arsenal_resolve::PrimitiveTy::U0 => Ty::U0,
        arsenal_resolve::PrimitiveTy::Bool => Ty::Bool,
        arsenal_resolve::PrimitiveTy::I8 => Ty::Int(IntTy::I8),
        arsenal_resolve::PrimitiveTy::I16 => Ty::Int(IntTy::I16),
        arsenal_resolve::PrimitiveTy::I32 => Ty::Int(IntTy::I32),
        arsenal_resolve::PrimitiveTy::I64 => Ty::Int(IntTy::I64),
        arsenal_resolve::PrimitiveTy::U8 => Ty::Int(IntTy::U8),
        arsenal_resolve::PrimitiveTy::U16 => Ty::Int(IntTy::U16),
        arsenal_resolve::PrimitiveTy::U32 => Ty::Int(IntTy::U32),
        arsenal_resolve::PrimitiveTy::U64 => Ty::Int(IntTy::U64),
        arsenal_resolve::PrimitiveTy::ISize => Ty::Int(IntTy::ISize),
        arsenal_resolve::PrimitiveTy::USize => Ty::Int(IntTy::USize),
        arsenal_resolve::PrimitiveTy::F32 => Ty::Float(FloatTy::F32),
        arsenal_resolve::PrimitiveTy::F64 => Ty::Float(FloatTy::F64),
        arsenal_resolve::PrimitiveTy::Rune => Ty::Rune,
    })
}

// ─── body checking ────────────────────────────────────────────────────

struct Cx<'a, 'tm, 'sm, 'd> {
    /// Source for slicing identifiers.
    sm: &'sm SourceMap,
    /// Diagnostic sink.
    diags: &'d mut DiagBag,
    /// Output module accumulator (we mutate `expr_types` /
    /// `call_targets` / `path_bindings` here).
    tm: &'tm mut TypedModule<'a>,
    /// Return type of the enclosing function, used by `return` checks.
    expected_ret: Ty,
    /// Local bindings — name → (binding-id, type), most recent first.
    /// Phase 1's scoping rules are flat-per-function: a `let` shadows
    /// the previous binding of the same name within the same function.
    /// Lexical scoping inside nested blocks is a Phase 1.5 enhancement.
    locals: Vec<(String, BindingId, Ty)>,
    /// Counter for the next [`BindingId`] within the current function.
    next_binding: u32,
    /// Depth of the enclosing-loop stack. Incremented on entering a
    /// `while` or `for` body; decremented on exit. Used by
    /// `synth_break` / `synth_continue` to reject usage outside a loop.
    loop_depth: u32,
}

impl<'a, 'tm, 'sm, 'd> Cx<'a, 'tm, 'sm, 'd> {
    fn alloc_binding(&mut self) -> BindingId {
        let id = BindingId(self.next_binding);
        self.next_binding += 1;
        id
    }

    fn lookup_local(&self, name: &str) -> Option<(BindingId, Ty)> {
        self.locals
            .iter()
            .rev()
            .find(|(n, _, _)| n == name)
            .map(|(_, id, ty)| (*id, *ty))
    }
}

fn check_fn_body<'a>(
    def_id: DefId,
    fn_decl: FnDecl<'a>,
    sm: &SourceMap,
    tm: &mut TypedModule<'a>,
    diags: &mut DiagBag,
) {
    let sig = tm.sigs.get(&def_id).cloned().unwrap_or(FnSig {
        params: Vec::new(),
        ret: Ty::Error,
    });
    let mut cx = Cx {
        sm,
        diags,
        tm,
        expected_ret: sig.ret,
        locals: Vec::new(),
        next_binding: 0,
        loop_depth: 0,
    };
    // Register parameters as bindings. We zip the FnSig::Param structs
    // (with resolved types) with the AST Param nodes so we can record
    // each binding under its CST node identity for MIR's BindingId →
    // Local plumbing.
    let ast_params: Vec<_> = fn_decl
        .params()
        .map(|pl| pl.params().collect::<Vec<_>>())
        .unwrap_or_default();
    for (sig_p, ast_p) in sig.params.iter().zip(ast_params.iter()) {
        let id = cx.alloc_binding();
        cx.locals.push((sig_p.name.clone(), id, sig_p.ty));
        cx.tm.pat_bindings.insert(NodePtr(ast_p.syntax()), id);
    }
    let Some(body) = fn_decl.body() else {
        // `extern fn foo(...) -> T;` — no body to check.
        return;
    };
    check_block(body, sig.ret, &mut cx);
}

/// Type-check a synthetic `main` body composed of the module's top-level
/// statements (Phase 1 increment 11a). The synthesised signature is
/// `fn () -> i32`; the implicit return-zero on fall-through is materialised
/// in MIR rather than typeck.
fn check_synthetic_main_body<'a>(
    module: Module<'a>,
    sm: &SourceMap,
    tm: &mut TypedModule<'a>,
    diags: &mut DiagBag,
) {
    let mut cx = Cx {
        sm,
        diags,
        tm,
        expected_ret: Ty::Int(IntTy::I32),
        locals: Vec::new(),
        next_binding: 0,
        loop_depth: 0,
    };
    for stmt in module.stmts() {
        check_stmt(stmt, &mut cx);
    }
}

fn check_block<'a>(block: Block<'a>, _expected: Ty, cx: &mut Cx<'a, '_, '_, '_>) {
    for stmt in block.stmts() {
        check_stmt(stmt, cx);
    }
}

fn check_stmt<'a>(stmt: Stmt<'a>, cx: &mut Cx<'a, '_, '_, '_>) {
    match stmt {
        Stmt::Let(l) => check_let(l, cx),
        Stmt::Expr(es) => check_expr_stmt(es, cx),
        Stmt::Stub(_) | Stmt::Error(_) => {}
    }
}

fn check_let<'a>(let_stmt: LetStmt<'a>, cx: &mut Cx<'a, '_, '_, '_>) {
    let annotated = let_stmt
        .ty()
        .map(|t| resolve_type(t, cx.tm.resolved, cx.sm, cx.diags));
    let init_ty = let_stmt.init().map(|e| {
        let expected = annotated.unwrap_or(Ty::Error);
        check_expr(e, expected, cx)
    });
    let final_ty = match (annotated, init_ty) {
        (Some(t), _) => t,
        (None, Some(t)) => t,
        (None, None) => Ty::Error,
    };
    // Bind the pattern.
    if let Some(Pattern::Ident(p)) = let_stmt.pattern() {
        if let Some(name_span) = p.name() {
            if let Some(name) = cx.sm.slice(name_span) {
                let id = cx.alloc_binding();
                cx.locals.push((name.to_string(), id, final_ty));
                cx.tm.pat_bindings.insert(NodePtr(p.syntax()), id);
            }
        }
    }
    // Wildcard pattern (`_`): allocate a binding id but don't add to
    // locals (no name to look up). Still record the mapping so MIR
    // can find a Local slot if it ever needs one.
    if let Some(Pattern::Wildcard(p)) = let_stmt.pattern() {
        let id = cx.alloc_binding();
        cx.tm.pat_bindings.insert(NodePtr(p.syntax()), id);
    }
}

fn check_expr_stmt<'a>(es: ExprStmt<'a>, cx: &mut Cx<'a, '_, '_, '_>) {
    if let Some(e) = es.expr() {
        // Top-level statement expressions are unconstrained — check in
        // synthesis mode.
        synth_expr(e, cx);
    }
}

/// Bidirectional checking. If `expected != Ty::Error`, the synthesised
/// type of `expr` must match it. The actual type of the expression is
/// recorded in `cx.tm.expr_types` and returned for caller convenience.
fn check_expr<'a>(expr: Expr<'a>, expected: Ty, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    let synth = synth_expr(expr, cx);
    if expected != Ty::Error && synth != Ty::Error && !ty_assignable(synth, expected) {
        cx.diags.push(Diagnostic::error(
            ec::TYPE_MISMATCH,
            Label::new(expr_span(expr), ""),
            format!("expected `{expected}`, found `{synth}`"),
        ));
    }
    synth
}

fn synth_expr<'a>(expr: Expr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    let node_ptr = NodePtr(expr.syntax());
    let ty = match expr {
        Expr::Literal(l) => synth_literal(l, cx),
        Expr::Path(p) => synth_path(p, cx),
        Expr::Paren(p) => synth_paren(p, cx),
        Expr::Binary(b) => synth_binary(b, cx),
        Expr::Unary(u) => synth_unary(u, cx),
        Expr::Block(b) => synth_block(b, cx),
        Expr::If(i) => synth_if(i, cx),
        Expr::While(w) => synth_while(w, cx),
        Expr::Return(r) => synth_return(r, cx),
        Expr::Call(c) => synth_call(c, cx),
        Expr::Break(br) => synth_break(br, cx),
        Expr::Continue(co) => synth_continue(co, cx),
        Expr::For(fe) => synth_for(fe, cx),
        Expr::StructLit(s) => synth_struct_lit(s, cx),
        Expr::Field(f) => synth_field(f, cx),
        Expr::Stub(n) | Expr::Error(n) => {
            cx.diags.push(Diagnostic::error(
                ec::UNSUPPORTED_CONSTRUCT,
                Label::new(n.span, ""),
                "this expression form is not yet supported by the Phase 1 type checker",
            ));
            Ty::Error
        }
    };
    cx.tm.expr_types.insert(node_ptr, ty);
    ty
}

fn synth_literal(l: LiteralExpr<'_>, _cx: &mut Cx<'_, '_, '_, '_>) -> Ty {
    match l.token_kind() {
        // Phase 1 literal-to-type mapping. Fancier inference (e.g.
        // ComptimeInt fitting any integer type) is a Phase 2 concern.
        Some(SyntaxKind::IntLit) => Ty::Int(IntTy::I32),
        Some(SyntaxKind::FloatLit) => Ty::Float(FloatTy::F64),
        Some(SyntaxKind::KwTrue) | Some(SyntaxKind::KwFalse) => Ty::Bool,
        Some(SyntaxKind::KwNil) => Ty::Error, // requires `?T` context
        Some(SyntaxKind::RuneLit) => Ty::Rune,
        Some(SyntaxKind::ByteCharLit) => Ty::Int(IntTy::U8),
        Some(SyntaxKind::StringLit | SyntaxKind::RawStringLit) => Ty::Slice(IntTy::U8),
        Some(SyntaxKind::CStringLit) => {
            // `c"..."` (null-terminated `[*:0]u8`) is a Phase 2 concern.
            Ty::Error
        }
        _ => Ty::Error,
    }
}

fn synth_path<'a>(path: PathExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    let mut iter = path.segments();
    let Some(first) = iter.next() else {
        return Ty::Error;
    };
    if iter.next().is_some() {
        // Multi-segment paths in expressions aren't supported in Phase 1.
        cx.diags.push(Diagnostic::error(
            ec::UNSUPPORTED_CONSTRUCT,
            Label::new(path.syntax().span, ""),
            "multi-segment paths in expressions are not yet supported",
        ));
        return Ty::Error;
    }
    let Some(name) = cx.sm.slice(first) else {
        return Ty::Error;
    };
    if let Some((id, ty)) = cx.lookup_local(name) {
        cx.tm.path_bindings.insert(NodePtr(path.syntax()), id);
        return ty;
    }
    // Maybe a top-level fn? In Phase 1 we don't yet model first-class
    // function values, but reporting cleanly here is preferable to a
    // generic "unknown name" if the user wrote `foo` instead of `foo()`.
    if cx.tm.resolved.lookup(name).is_some() {
        cx.diags.push(Diagnostic::error(
            ec::UNSUPPORTED_CONSTRUCT,
            Label::new(path.syntax().span, ""),
            format!(
                "function `{name}` referenced here without a call; first-class function values are not yet supported"
            ),
        ));
        return Ty::Error;
    }
    cx.diags.push(Diagnostic::error(
        ec::UNKNOWN_NAME,
        Label::new(path.syntax().span, ""),
        format!("unknown name `{name}`"),
    ));
    Ty::Error
}

fn synth_paren<'a>(p: ParenExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    p.inner().map(|e| synth_expr(e, cx)).unwrap_or(Ty::Error)
}

fn synth_binary<'a>(b: BinaryExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    // Assignment is structurally a BinaryExpr (the parser wraps every
    // infix uniformly) but semantically a place-update statement. Peel
    // it off before synthesising operands to avoid asking for the
    // value-type of the LHS path expression in a context where it will
    // be written, not read.
    if matches!(b.op_kind(), Some(SyntaxKind::Eq)) {
        return synth_assign(b, cx);
    }
    let lhs = b.lhs().map(|e| synth_expr(e, cx)).unwrap_or(Ty::Error);
    let rhs = b.rhs().map(|e| synth_expr(e, cx)).unwrap_or(Ty::Error);
    let Some(op) = b.op_kind() else {
        return Ty::Error;
    };
    use SyntaxKind::*;
    match op {
        Plus | Minus | Star | Slash | Percent | StarStar => {
            require_same_numeric(lhs, rhs, b.syntax().span, op_label(op), cx)
        }
        Amp | Pipe | Caret | LtLt | GtGt => {
            require_same_int(lhs, rhs, b.syntax().span, op_label(op), cx)
        }
        AmpAmp | PipePipe => {
            require_bool(lhs, b.syntax().span, op_label(op), cx);
            require_bool(rhs, b.syntax().span, op_label(op), cx);
            Ty::Bool
        }
        EqEq | BangEq => {
            require_assignable(lhs, rhs, b.syntax().span, op_label(op), cx);
            Ty::Bool
        }
        Lt | LtEq | Gt | GtEq => {
            require_same_numeric(lhs, rhs, b.syntax().span, op_label(op), cx);
            Ty::Bool
        }
        _ => Ty::Error,
    }
}

/// `x = expr` where `x` is a path to a local. The Phase-1 model
/// treats every let-bound local as mutable; `let` vs `var` is a
/// borrow-checker concern and lands in Phase 3.
fn synth_assign<'a>(b: BinaryExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    let lhs_ty = match b.lhs() {
        Some(Expr::Path(p)) => synth_path(p, cx),
        Some(Expr::Field(fe)) => synth_field(fe, cx),
        Some(other) => {
            cx.diags.push(Diagnostic::error(
                ec::BAD_OPERAND,
                Label::new(expr_span(other), ""),
                "left side of `=` must be an assignable place (a local variable name or field access)",
            ));
            Ty::Error
        }
        None => Ty::Error,
    };
    if let Some(rhs) = b.rhs() {
        check_expr(rhs, lhs_ty, cx);
    }
    Ty::U0
}

fn synth_unary<'a>(u: UnaryExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    let operand = u.operand().map(|e| synth_expr(e, cx)).unwrap_or(Ty::Error);
    let Some(op) = u.op_kind() else {
        return Ty::Error;
    };
    use SyntaxKind::*;
    match op {
        Minus => {
            if operand.is_numeric() || operand == Ty::Error {
                operand
            } else {
                cx.diags.push(Diagnostic::error(
                    ec::BAD_OPERAND,
                    Label::new(u.syntax().span, ""),
                    format!("unary `-` requires a numeric operand, found `{operand}`"),
                ));
                Ty::Error
            }
        }
        Bang => {
            require_bool(operand, u.syntax().span, "!", cx);
            Ty::Bool
        }
        Tilde => {
            if operand.is_integer() || operand == Ty::Error {
                operand
            } else {
                cx.diags.push(Diagnostic::error(
                    ec::BAD_OPERAND,
                    Label::new(u.syntax().span, ""),
                    format!("unary `~` requires an integer operand, found `{operand}`"),
                ));
                Ty::Error
            }
        }
        _ => Ty::Error,
    }
}

fn synth_block<'a>(block: Block<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    for stmt in block.stmts() {
        check_stmt(stmt, cx);
    }
    if let Some(tail) = block.tail_expr() {
        synth_expr(tail, cx)
    } else {
        Ty::U0
    }
}

fn synth_if<'a>(i: IfExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    if let Some(cond) = i.cond() {
        check_expr(cond, Ty::Bool, cx);
    }
    let then_ty = i.then_block().map(|b| synth_block(b, cx)).unwrap_or(Ty::U0);
    if let Some(else_branch) = i.else_branch() {
        let else_ty = synth_expr(else_branch, cx);
        if then_ty != else_ty && then_ty != Ty::Error && else_ty != Ty::Error {
            cx.diags.push(Diagnostic::error(
                ec::TYPE_MISMATCH,
                Label::new(i.syntax().span, ""),
                format!("`if` branches have incompatible types: `{then_ty}` and `{else_ty}`"),
            ));
            return Ty::Error;
        }
        then_ty
    } else {
        Ty::U0
    }
}

fn synth_while<'a>(w: WhileExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    if let Some(cond) = w.cond() {
        check_expr(cond, Ty::Bool, cx);
    }
    if let Some(body) = w.body() {
        cx.loop_depth += 1;
        synth_block(body, cx);
        cx.loop_depth -= 1;
    }
    Ty::U0
}

fn synth_break<'a>(br: BreakExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    if cx.loop_depth == 0 {
        cx.diags.push(Diagnostic::error(
            ec::BREAK_OUTSIDE_LOOP,
            Label::new(br.syntax().span, ""),
            "`break` outside of a loop",
        ));
    }
    if let Some(v) = br.value() {
        // Phase 1 doesn't thread break values; we still type-check the
        // expression so user typos get diagnosed.
        let _ = synth_expr(v, cx);
    }
    Ty::U0
}

fn synth_continue<'a>(co: ContinueExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    if cx.loop_depth == 0 {
        cx.diags.push(Diagnostic::error(
            ec::BREAK_OUTSIDE_LOOP,
            Label::new(co.syntax().span, ""),
            "`continue` outside of a loop",
        ));
    }
    Ty::U0
}

fn synth_for<'a>(fe: ForExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    // Range bounds. Phase 1 supports only integer ranges; we infer the
    // loop variable's type from the *start* bound and check the end
    // against it.
    let start_ty = match fe.range_start() {
        Some(e) => synth_expr(e, cx),
        None => Ty::Error,
    };
    if let Some(e) = fe.range_end() {
        check_expr(e, start_ty, cx);
    }
    let var_ty = if start_ty.is_integer() {
        start_ty
    } else if start_ty == Ty::Error {
        Ty::Error
    } else {
        cx.diags.push(Diagnostic::error(
            ec::BAD_OPERAND,
            Label::new(fe.syntax().span, ""),
            format!("`for` loop range must be integer, found `{start_ty}`"),
        ));
        Ty::Error
    };
    // Bind the loop variable for the body's scope. We snapshot the
    // previous local-vec length so we can pop the binding afterwards.
    let saved_len = cx.locals.len();
    if let Some(Pattern::Ident(p)) = fe.pattern() {
        if let Some(name_span) = p.name() {
            if let Some(name) = cx.sm.slice(name_span) {
                let id = cx.alloc_binding();
                cx.locals.push((name.to_string(), id, var_ty));
                cx.tm.pat_bindings.insert(NodePtr(p.syntax()), id);
            }
        }
    }
    if let Some(Pattern::Wildcard(p)) = fe.pattern() {
        let id = cx.alloc_binding();
        cx.tm.pat_bindings.insert(NodePtr(p.syntax()), id);
    }
    if let Some(body) = fe.body() {
        cx.loop_depth += 1;
        synth_block(body, cx);
        cx.loop_depth -= 1;
    }
    cx.locals.truncate(saved_len);
    Ty::U0
}

fn synth_struct_lit<'a>(s: StructLitExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    // Resolve the class name.
    let path = match s.path() {
        Some(p) => p,
        None => return Ty::Error,
    };
    let mut segs = path.segments();
    let Some(first) = segs.next() else {
        return Ty::Error;
    };
    if segs.next().is_some() {
        cx.diags.push(Diagnostic::error(
            ec::UNSUPPORTED_CONSTRUCT,
            Label::new(path.syntax().span, ""),
            "multi-segment paths in struct literals are not yet supported",
        ));
        return Ty::Error;
    }
    let Some(name) = cx.sm.slice(first) else {
        return Ty::Error;
    };
    let def = match cx.tm.resolved.lookup(name) {
        Some(d) if d.kind == DefKind::Class => d.id,
        Some(_) => {
            cx.diags.push(Diagnostic::error(
                ec::NOT_A_CLASS,
                Label::new(path.syntax().span, ""),
                format!("`{name}` is not a class"),
            ));
            return Ty::Error;
        }
        None => {
            cx.diags.push(Diagnostic::error(
                ec::UNKNOWN_NAME,
                Label::new(path.syntax().span, ""),
                format!("unknown class `{name}`"),
            ));
            return Ty::Error;
        }
    };
    let layout = cx.tm.classes.get(&def).cloned().unwrap_or(ClassLayout {
        name: name.to_string(),
        fields: Vec::new(),
    });

    // Walk the literal's `.field = expr` items. Track which declared
    // fields have been initialised; warn on duplicates and missing.
    let mut seen = vec![false; layout.fields.len()];
    if let Some(list) = s.fields() {
        for fld in list.fields() {
            let lit_name_span = fld.name();
            let lit_name = lit_name_span
                .and_then(|sp| cx.sm.slice(sp).map(str::to_string))
                .unwrap_or_default();
            let idx_match = layout.fields.iter().position(|f| f.name == lit_name);
            let expected_ty = match idx_match {
                Some(i) => {
                    if seen[i] {
                        cx.diags.push(Diagnostic::error(
                            ec::FIELD_INIT_MISMATCH,
                            Label::new(fld.syntax().span, ""),
                            format!("field `{lit_name}` initialised more than once"),
                        ));
                    }
                    seen[i] = true;
                    layout.fields[i].ty
                }
                None => {
                    cx.diags.push(Diagnostic::error(
                        ec::UNKNOWN_FIELD,
                        Label::new(lit_name_span.unwrap_or_else(|| fld.syntax().span), ""),
                        format!("class `{}` has no field `{lit_name}`", layout.name),
                    ));
                    Ty::Error
                }
            };
            if let Some(value) = fld.value() {
                check_expr(value, expected_ty, cx);
            }
        }
    }
    // Diagnose missing fields.
    let missing: Vec<_> = layout
        .fields
        .iter()
        .zip(seen.iter())
        .filter_map(|(f, &init)| (!init).then_some(f.name.as_str()))
        .collect();
    if !missing.is_empty() {
        cx.diags.push(Diagnostic::error(
            ec::FIELD_INIT_MISMATCH,
            Label::new(s.syntax().span, ""),
            format!("struct literal is missing field(s): {}", missing.join(", "),),
        ));
    }

    Ty::Class(def)
}

fn synth_field<'a>(fe: FieldExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    let base_ty = match fe.base() {
        Some(b) => synth_expr(b, cx),
        None => return Ty::Error,
    };
    let Some(name_span) = fe.field_name() else {
        return Ty::Error;
    };
    let Some(name) = cx.sm.slice(name_span) else {
        return Ty::Error;
    };
    if let Ty::Slice(elem) = base_ty {
        // Phase 1 slice fields: `len` is exposed (usize); `data` is
        // a raw pointer to the slice's element type (added in
        // increment 11c). Other names are genuinely unknown.
        return match name {
            "len" => Ty::Int(IntTy::USize),
            "data" => Ty::Ptr(elem),
            _ => {
                cx.diags.push(Diagnostic::error(
                    ec::UNKNOWN_FIELD,
                    Label::new(name_span, ""),
                    format!("slice has no field `{name}`"),
                ));
                Ty::Error
            }
        };
    }
    let class_id = match base_ty {
        Ty::Class(id) => id,
        Ty::Error => return Ty::Error,
        other => {
            cx.diags.push(Diagnostic::error(
                ec::FIELD_ON_NON_CLASS,
                Label::new(fe.syntax().span, ""),
                format!("field access on non-class type `{other}`"),
            ));
            return Ty::Error;
        }
    };
    let layout = match cx.tm.classes.get(&class_id) {
        Some(l) => l.clone(),
        None => return Ty::Error,
    };
    match layout.fields.iter().find(|f| f.name == name) {
        Some(f) => f.ty,
        None => {
            cx.diags.push(Diagnostic::error(
                ec::UNKNOWN_FIELD,
                Label::new(name_span, ""),
                format!("class `{}` has no field `{name}`", layout.name),
            ));
            Ty::Error
        }
    }
}

fn synth_return<'a>(r: ReturnExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    let expected = cx.expected_ret;
    match r.value() {
        Some(e) => {
            check_expr(e, expected, cx);
        }
        None => {
            if expected != Ty::U0 && expected != Ty::Error {
                cx.diags.push(Diagnostic::error(
                    ec::RETURN_VALUE_MISMATCH,
                    Label::new(r.syntax().span, ""),
                    format!("`return` without a value, but the function returns `{expected}`"),
                ));
            }
        }
    }
    // `return` is divergent; for typing purposes it matches anything.
    expected
}

fn synth_call<'a>(c: CallExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    // Determine the callee. Phase 1 only supports calling a
    // top-level fn by name (PathExpr → DefId).
    let Some(Expr::Path(callee_path)) = c.callee() else {
        cx.diags.push(Diagnostic::error(
            ec::UNSUPPORTED_CONSTRUCT,
            Label::new(c.syntax().span, ""),
            "Phase 1 only supports calls to top-level functions by name",
        ));
        // Still synthesise argument types so we record them.
        if let Some(args) = c.args() {
            for a in args.args() {
                synth_expr(a, cx);
            }
        }
        return Ty::Error;
    };
    let mut segs = callee_path.segments();
    let Some(first) = segs.next() else {
        return Ty::Error;
    };
    if segs.next().is_some() {
        cx.diags.push(Diagnostic::error(
            ec::UNSUPPORTED_CONSTRUCT,
            Label::new(callee_path.syntax().span, ""),
            "multi-segment callee paths are not yet supported",
        ));
        return Ty::Error;
    }
    let name = cx.sm.slice(first).unwrap_or("");
    let def = match cx.tm.resolved.lookup(name) {
        Some(d) => d,
        None => {
            cx.diags.push(Diagnostic::error(
                ec::UNKNOWN_NAME,
                Label::new(callee_path.syntax().span, ""),
                format!("unknown function `{name}`"),
            ));
            return Ty::Error;
        }
    };
    let sig = cx.tm.sigs.get(&def.id).cloned().unwrap();
    cx.tm.call_targets.insert(NodePtr(c.syntax()), def.id);
    let args: Vec<_> = c
        .args()
        .map(|a| a.args().collect::<Vec<_>>())
        .unwrap_or_default();
    if args.len() != sig.params.len() {
        cx.diags.push(Diagnostic::error(
            ec::WRONG_ARG_COUNT,
            Label::new(c.syntax().span, ""),
            format!(
                "`{name}` takes {expected} argument(s), but {got} were given",
                expected = sig.params.len(),
                got = args.len(),
            ),
        ));
        for a in &args {
            synth_expr(*a, cx);
        }
        return sig.ret;
    }
    for (a, p) in args.iter().zip(sig.params.iter()) {
        check_expr(*a, p.ty, cx);
    }
    sig.ret
}

// ─── helpers ───────────────────────────────────────────────────────────

fn ty_assignable(actual: Ty, expected: Ty) -> bool {
    actual == expected || actual == Ty::Error || expected == Ty::Error
}

fn require_same_numeric(lhs: Ty, rhs: Ty, span: Span, op: &str, cx: &mut Cx<'_, '_, '_, '_>) -> Ty {
    if !lhs.is_numeric() && lhs != Ty::Error {
        cx.diags.push(Diagnostic::error(
            ec::BAD_OPERAND,
            Label::new(span, ""),
            format!("operator `{op}` requires numeric operands; left side is `{lhs}`"),
        ));
        return Ty::Error;
    }
    if !rhs.is_numeric() && rhs != Ty::Error {
        cx.diags.push(Diagnostic::error(
            ec::BAD_OPERAND,
            Label::new(span, ""),
            format!("operator `{op}` requires numeric operands; right side is `{rhs}`"),
        ));
        return Ty::Error;
    }
    if lhs == rhs {
        lhs
    } else if lhs == Ty::Error {
        rhs
    } else if rhs == Ty::Error {
        lhs
    } else {
        cx.diags.push(Diagnostic::error(
            ec::TYPE_MISMATCH,
            Label::new(span, ""),
            format!(
                "operator `{op}` requires both operands to have the same type, found `{lhs}` and `{rhs}`"
            ),
        ));
        Ty::Error
    }
}

fn require_same_int(lhs: Ty, rhs: Ty, span: Span, op: &str, cx: &mut Cx<'_, '_, '_, '_>) -> Ty {
    if !lhs.is_integer() && lhs != Ty::Error {
        cx.diags.push(Diagnostic::error(
            ec::BAD_OPERAND,
            Label::new(span, ""),
            format!("operator `{op}` requires integer operands; left side is `{lhs}`"),
        ));
        return Ty::Error;
    }
    if !rhs.is_integer() && rhs != Ty::Error {
        cx.diags.push(Diagnostic::error(
            ec::BAD_OPERAND,
            Label::new(span, ""),
            format!("operator `{op}` requires integer operands; right side is `{rhs}`"),
        ));
        return Ty::Error;
    }
    if lhs == rhs {
        lhs
    } else if lhs == Ty::Error {
        rhs
    } else if rhs == Ty::Error {
        lhs
    } else {
        cx.diags.push(Diagnostic::error(
            ec::TYPE_MISMATCH,
            Label::new(span, ""),
            format!(
                "operator `{op}` requires both operands to have the same type, found `{lhs}` and `{rhs}`"
            ),
        ));
        Ty::Error
    }
}

fn require_bool(t: Ty, span: Span, op: &str, cx: &mut Cx<'_, '_, '_, '_>) {
    if t != Ty::Bool && t != Ty::Error {
        cx.diags.push(Diagnostic::error(
            ec::BAD_OPERAND,
            Label::new(span, ""),
            format!("operator `{op}` requires a `bool`, found `{t}`"),
        ));
    }
}

fn require_assignable(lhs: Ty, rhs: Ty, span: Span, op: &str, cx: &mut Cx<'_, '_, '_, '_>) {
    if lhs != rhs && lhs != Ty::Error && rhs != Ty::Error {
        cx.diags.push(Diagnostic::error(
            ec::TYPE_MISMATCH,
            Label::new(span, ""),
            format!(
                "operator `{op}` requires both sides to have the same type, found `{lhs}` and `{rhs}`"
            ),
        ));
    }
}

fn op_label(op: SyntaxKind) -> &'static str {
    use SyntaxKind::*;
    match op {
        Plus => "+",
        Minus => "-",
        Star => "*",
        Slash => "/",
        Percent => "%",
        StarStar => "**",
        EqEq => "==",
        BangEq => "!=",
        Lt => "<",
        LtEq => "<=",
        Gt => ">",
        GtEq => ">=",
        Amp => "&",
        Pipe => "|",
        Caret => "^",
        AmpAmp => "&&",
        PipePipe => "||",
        LtLt => "<<",
        GtGt => ">>",
        _ => "<op>",
    }
}

fn expr_span(e: Expr<'_>) -> Span {
    match e {
        Expr::Literal(x) => x.syntax().span,
        Expr::Path(x) => x.syntax().span,
        Expr::Paren(x) => x.syntax().span,
        Expr::Binary(x) => x.syntax().span,
        Expr::Unary(x) => x.syntax().span,
        Expr::Block(x) => x.syntax().span,
        Expr::If(x) => x.syntax().span,
        Expr::While(x) => x.syntax().span,
        Expr::Return(x) => x.syntax().span,
        Expr::Call(x) => x.syntax().span,
        Expr::Break(x) => x.syntax().span,
        Expr::Continue(x) => x.syntax().span,
        Expr::For(x) => x.syntax().span,
        Expr::StructLit(x) => x.syntax().span,
        Expr::Field(x) => x.syntax().span,
        Expr::Stub(n) | Expr::Error(n) => n.span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arsenal_ast::FileArena;
    use arsenal_lex::SourceMap;
    use arsenal_parse::parse;
    use arsenal_resolve::resolve_module;
    use bumpalo::Bump;

    fn check(src: &str) -> u32 {
        let mut sm = SourceMap::new();
        let file = sm.add_file("t.gw", src);
        let bytes = sm.get(file).unwrap().contents.as_bytes();
        let bump = Bump::new();
        let arena = FileArena::new(&bump, file);
        let (root, mut diags) = parse(file, bytes, &arena);
        let resolved = resolve_module(root, &sm, &mut diags);
        let _typed = type_check(&resolved, &sm, &mut diags);
        diags.error_count()
    }

    #[test]
    fn empty_main_is_clean() {
        assert_eq!(check("fn main() -> i32 { return 0; }"), 0);
    }

    #[test]
    fn return_type_mismatch_diagnoses() {
        // returns true (bool) but signature says i32
        assert!(check("fn f() -> i32 { return true; }") >= 1);
    }

    #[test]
    fn unknown_type_diagnoses() {
        assert!(check("fn f() -> NotAType { return 0; }") >= 1);
    }

    #[test]
    fn binary_arith_clean_with_explicit_type() {
        assert_eq!(check("fn f() -> i32 { return 1 + 2; }"), 0);
    }

    #[test]
    fn comparison_yields_bool() {
        assert_eq!(check("fn f() -> bool { return 1 < 2; }"), 0);
    }

    #[test]
    fn call_arg_count_mismatch_diagnoses() {
        let src =
            "fn add(x: i32, y: i32) -> i32 { return x + y; } fn f() -> i32 { return add(1); }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn call_arg_type_mismatch_diagnoses() {
        let src = "fn id(x: i32) -> i32 { return x; } fn f() -> i32 { return id(true); }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn unknown_name_in_expr_diagnoses() {
        assert!(check("fn f() -> i32 { return notbound; }") >= 1);
    }

    #[test]
    fn let_binds_type() {
        assert_eq!(check("fn f() -> i32 { let x: i32 = 1; return x; }"), 0);
    }

    // ─── Phase 1 increment 11b: slice type ────────────────────────────────

    #[test]
    fn slice_display_format() {
        assert_eq!(format!("{}", Ty::Slice(IntTy::U8)), "[]u8");
    }

    #[test]
    fn string_lit_types_as_slice_u8() {
        // `let s: []u8 = "hi";` should typecheck cleanly with no errors.
        assert_eq!(
            check(r#"fn f() -> i32 { let s: []u8 = "hi"; return 0; }"#),
            0
        );
    }

    #[test]
    fn slice_len_is_usize() {
        // `s.len` flowing into a `usize` binding must agree on type.
        let src = r#"fn f() -> i32 { let s: []u8 = "abc"; let n: usize = s.len; return 0; }"#;
        // Note: `n: usize` rejects `s.len` if `.len` returned anything
        // other than usize. This test will fail (not 0 errors) until the
        // typing path lands.
        assert_eq!(check(src), 0);
    }

    #[test]
    fn non_u8_slice_element_diagnoses() {
        assert!(check("fn f() -> i32 { let s: []i32; return 0; }") >= 1);
    }

    #[test]
    fn slice_typed_param_diagnoses() {
        assert!(check("fn g(s: []u8) -> i32 { return 0; }") >= 1);
    }

    // ─── Phase 1 increment 11c: raw pointers + implicit Print ─────────────

    #[test]
    fn ptr_display_format() {
        assert_eq!(format!("{}", Ty::Ptr(IntTy::U8)), "*u8");
        assert_eq!(format!("{}", Ty::Ptr(IntTy::I8)), "*i8");
    }

    #[test]
    fn ptr_in_extern_fn_sig_is_clean() {
        let src =
            "extern fn write(fd: i32, buf: *u8, count: usize) -> isize; fn f() -> i32 { return 0; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn ptr_in_non_extern_fn_param_diagnoses() {
        // Raw pointers are FFI-only in Phase 1; bare `fn` rejects them.
        assert!(check("fn g(p: *u8) -> i32 { return 0; }") >= 1);
    }

    #[test]
    fn ptr_in_non_extern_fn_return_diagnoses() {
        assert!(check("fn g() -> *u8 { return 0; }") >= 1);
    }

    #[test]
    fn slice_data_typed_as_ptr_u8() {
        // `extern fn write` accepts `*u8`; `slice.data` must produce
        // exactly that type for the call to typecheck.
        let src = r#"
            extern fn write(fd: i32, buf: *u8, count: usize) -> isize;
            fn f() -> i32 {
                let s: []u8 = "hi";
                write(1, s.data, s.len);
                return 0;
            }
        "#;
        assert_eq!(check(src), 0);
    }

    #[test]
    fn non_byte_pointee_diagnoses() {
        assert!(check("extern fn f(p: *i32) -> i32;") >= 1);
    }
}
