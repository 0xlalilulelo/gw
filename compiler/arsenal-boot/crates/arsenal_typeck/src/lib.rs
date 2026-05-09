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
    AstNode, BinaryExpr, Block, BreakExpr, CallExpr, CastExpr, ClassDecl, ContinueExpr, Expr,
    ExprStmt, FieldExpr, FnDecl, ForExpr, IfExpr, LetStmt, LiteralExpr, MatchExpr, Module,
    ParenExpr, PathExpr, PathType, Pattern, ReturnExpr, Stmt, StructLitExpr, SyntaxKind,
    SyntaxNode, Type, UnaryExpr, WhileExpr,
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
    // 307 retired (was MISSING_RETURN_TYPE; missing `-> T` now defaults to `u0`).
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
    /// `[*:S]T` — sentinel-terminated many-pointer (Phase 2 increment
    /// C.2). Distinct from `*T` at the type level, identical to it at
    /// the value level. Phase 2 only realises `[*:0]u8` (the type of
    /// `c"..."` literals); the typeck layer rejects other element types
    /// or sentinels until the corpus motivates more. The
    /// `[*:S]T → *T` direction is permitted by `ty_assignable` so
    /// `c"..."` flows into existing `extern fn x(*u8)` slots without
    /// the user writing an explicit cast.
    SentinelPtr {
        /// Element type. Phase 2 only realises `IntTy::U8`.
        elem: IntTy,
        /// Sentinel value the producer guarantees terminates the run.
        /// Phase 2 only realises `0`.
        sentinel: u64,
    },
    /// `?T` — optional value (Phase 2 increment O.1). Lowers as a
    /// 2-field aggregate `{ tag: u8, payload: T }`; tag = 0 means
    /// nil, tag = 1 means a populated payload. The closed
    /// [`OptInner`] keeps `Ty` Copy + non-Box; Phase 2 minimum
    /// realises `?Int(IntTy)` and `?Bool` only — wider inner types
    /// (classes, slices, pointers) ride later sub-bundles.
    Optional(OptInner),
    /// Synthetic placeholder when type checking failed for an
    /// expression. Treated as compatible with any expected type so a
    /// single failure does not cascade.
    Error,
}

/// The inner of a [`Ty::Optional`]. Closed enum to keep `Ty` Copy.
/// Phase 2 minimum realises only the integer and bool primitives —
/// wider inner types (classes, slices, pointers) need additional
/// codegen layout work and ride later sub-bundles.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum OptInner {
    /// `?i32`, `?u64`, etc.
    Int(IntTy),
    /// `?bool`.
    Bool,
}

impl OptInner {
    /// Promote the closed inner to a full [`Ty`] for paths that need
    /// the wider form (codegen layout, MIR field types, etc.).
    pub fn to_ty(self) -> Ty {
        match self {
            Self::Int(t) => Ty::Int(t),
            Self::Bool => Ty::Bool,
        }
    }

    /// Try to demote a [`Ty`] to its [`OptInner`] form. Returns `None`
    /// for any non-supported inner (the typeck rejects those at
    /// `resolve_type` time, but the helper is convenient for MIR /
    /// codegen).
    pub fn from_ty(ty: Ty) -> Option<Self> {
        match ty {
            Ty::Int(t) => Some(Self::Int(t)),
            Ty::Bool => Some(Self::Bool),
            _ => None,
        }
    }
}

impl std::fmt::Display for OptInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_ty().fmt(f)
    }
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
            Self::SentinelPtr { elem, sentinel } => {
                write!(f, "[*:{sentinel}]{}", Self::Int(*elem))
            }
            Self::Optional(inner) => write!(f, "?{}", inner),
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
            // Class- and slice-typed params share the by-pointer ABI
            // (classes landed in A.3, slices in A.4 — both flow as
            // aggregates through `is_aggregate_ty` in codegen).
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
    // A missing return type defaults to `u0` (spec §5.4.1) — matches
    // the elided form `fn f(...) { ... }` used in helpers that do I/O
    // for side effects only. Out of grammar there is no syntactic
    // distinction between "explicit `-> u0`" and "no `->` at all".
    let ret = match fn_decl.ret_type().and_then(|rt| rt.ty()) {
        Some(t) => resolve_type(t, resolved, sm, diags),
        None => Ty::U0,
    };
    // Class- and slice-typed returns are accepted via the by-pointer
    // ABI (hidden out-pointer; classes A.3, slices A.4). Pointers
    // remain extern-only until the memory model lands.
    if matches!(ret, Ty::Ptr(_)) && !is_extern {
        diags.push(Diagnostic::error(
            ec::UNSUPPORTED_CONSTRUCT,
            Label::new(fn_decl.span(), ""),
            "raw pointer return types are only allowed in `extern fn` declarations in Phase 1",
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
        Type::Opt(opt) => {
            // Phase 2 increment O.1: `?T` for primitive T (`Ty::Int(_)`
            // or `Ty::Bool`). Wider inner types (classes, slices,
            // pointers, sentinel-pointers, nested Optionals) need
            // additional codegen layout work; reject them here so
            // bigger inner-type plumbing can ride a separate
            // sub-bundle.
            let inner_ty = opt
                .inner()
                .map(|t| resolve_type(t, resolved, sm, diags))
                .unwrap_or(Ty::Error);
            return match OptInner::from_ty(inner_ty) {
                Some(oi) => Ty::Optional(oi),
                None if inner_ty == Ty::Error => Ty::Error,
                None => {
                    diags.push(Diagnostic::error(
                        ec::UNSUPPORTED_CONSTRUCT,
                        Label::new(opt.syntax().span, ""),
                        format!(
                            "Phase 2 only supports `?T` for primitive integer or `bool` inner types; `?{inner_ty}` is not yet supported"
                        ),
                    ));
                    Ty::Error
                }
            };
        }
        Type::SentinelPtr(s) => {
            // Phase 2 increment C.2: the corpus only needs `[*:0]u8`
            // (the c-string type). Any other element type or sentinel
            // value rejects with `UNSUPPORTED_CONSTRUCT` so callers
            // see why their type didn't take.
            let elem_ty = s
                .element()
                .map(|e| resolve_type(e, resolved, sm, diags))
                .unwrap_or(Ty::Error);
            let sentinel = s.sentinel().and_then(|e| literal_u64(e, sm));
            return match (elem_ty, sentinel) {
                (Ty::Int(IntTy::U8), Some(0)) => Ty::SentinelPtr {
                    elem: IntTy::U8,
                    sentinel: 0,
                },
                (Ty::Error, _) => Ty::Error,
                (other_elem, Some(other_sent)) => {
                    diags.push(Diagnostic::error(
                        ec::UNSUPPORTED_CONSTRUCT,
                        Label::new(s.syntax().span, ""),
                        format!(
                            "Phase 2 only supports `[*:0]u8` sentinel pointers; `[*:{other_sent}]{other_elem}` is not yet supported"
                        ),
                    ));
                    Ty::Error
                }
                (_, None) => {
                    diags.push(Diagnostic::error(
                        ec::UNSUPPORTED_CONSTRUCT,
                        Label::new(s.syntax().span, ""),
                        "sentinel-pointer sentinel must be an integer literal in Phase 2 (only `[*:0]u8` is wired up)",
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
        Type::SentinelPtr(p) => p.syntax().span,
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
    // Bidirectional narrowing for unsuffixed numeric literals: if the
    // caller wants an integer type and the expression is an integer
    // literal whose value fits, type the literal at the expected
    // width. Same for float literals against a float context. This is
    // the minimum bidirectional inference Phase 1 needs so that
    // `let n: i64 = 5;` and `fact(10): i64` work without an `as` cast.
    if let Some(narrowed) = try_narrow_literal(expr, expected, cx) {
        cx.tm.expr_types.insert(NodePtr(expr.syntax()), narrowed);
        return narrowed;
    }
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

/// If `expr` is an unsuffixed integer or float literal and `expected`
/// is a compatible numeric type, return the narrowed type. Emits a
/// diagnostic and returns `Some(Ty::Error)` if the integer literal
/// does not fit the requested width. Returns `None` for any other
/// shape (compound expressions, mismatched kinds, error contexts) so
/// the caller falls back to the default synthesise-then-check path.
///
/// Negative literals (`-7`) parse as `Unary(Minus, IntLit)`; this
/// helper recognises that shape and parses the literal as a negative
/// value, so `let n: i64 = -100;` and `if x == -100` both narrow.
/// Float negation works the same way.
fn try_narrow_literal<'a>(expr: Expr<'a>, expected: Ty, cx: &mut Cx<'a, '_, '_, '_>) -> Option<Ty> {
    if expected == Ty::Error {
        return None;
    }
    // Phase 2 O.1: bare `nil` adopts `?T` whenever a `?T` context is
    // expected. Outside a `?T` context `nil` synthesises as
    // `Ty::Error` (the existing fallback in `synth_literal`); typeck
    // diagnoses `nil` in any non-Optional slot.
    if let Expr::Literal(l) = expr {
        if matches!(l.token_kind(), Some(SyntaxKind::KwNil)) {
            if let Ty::Optional(_) = expected {
                cx.tm.expr_types.insert(NodePtr(l.syntax()), expected);
                return Some(expected);
            }
        }
    }
    // Recognise both bare `Literal` and `Unary(Minus, Literal)` shapes.
    // Paren wrappers also pass through so `(-7)` narrows like `-7`.
    let (lit, negate, outer_span) = match expr {
        Expr::Literal(l) => (l, false, l.syntax().span),
        Expr::Unary(u) => match (u.op_kind(), u.operand()) {
            (Some(SyntaxKind::Minus), Some(Expr::Literal(l))) => (l, true, u.syntax().span),
            _ => return None,
        },
        Expr::Paren(p) => return try_narrow_literal(p.inner()?, expected, cx),
        _ => return None,
    };
    let (kind, span) = lit.token()?;
    match (kind, expected) {
        (SyntaxKind::IntLit, Ty::Int(target)) => {
            let raw = cx.sm.slice(span)?;
            let mut value = parse_unsigned_int_literal(raw)?;
            if negate {
                value = -value;
            }
            if value_fits_int(value, target) {
                if negate {
                    // Record the inner literal's type too, so MIR's
                    // `lower_literal` produces the right `IntTy`.
                    cx.tm
                        .expr_types
                        .insert(NodePtr(lit.syntax()), Ty::Int(target));
                }
                Some(Ty::Int(target))
            } else {
                let display = if negate {
                    format!("-{raw}")
                } else {
                    raw.to_string()
                };
                cx.diags.push(Diagnostic::error(
                    ec::TYPE_MISMATCH,
                    Label::new(outer_span, ""),
                    format!("integer literal `{display}` does not fit in `{expected}`"),
                ));
                Some(Ty::Error)
            }
        }
        (SyntaxKind::FloatLit, Ty::Float(target)) => {
            if negate {
                cx.tm
                    .expr_types
                    .insert(NodePtr(lit.syntax()), Ty::Float(target));
            }
            Some(Ty::Float(target))
        }
        _ => None,
    }
}

/// Extract a non-negative integer literal value from `expr` as `u64`.
/// Used by `resolve_type` for the sentinel slot of `[*:S]T` (Phase 2
/// C.2). Returns `None` for any non-`IntLit` shape, for negative
/// literals, or for values that overflow `u64`.
fn literal_u64(expr: Expr<'_>, sm: &SourceMap) -> Option<u64> {
    let lit = match expr {
        Expr::Literal(l) => l,
        Expr::Paren(p) => return literal_u64(p.inner()?, sm),
        _ => return None,
    };
    let (kind, span) = lit.token()?;
    if kind != SyntaxKind::IntLit {
        return None;
    }
    let raw = sm.slice(span)?;
    let v = parse_unsigned_int_literal(raw)?;
    if v < 0 {
        return None;
    }
    u64::try_from(v).ok()
}

/// Parse the source text of an `IntLit` token. Token text is always
/// non-negative (unary minus is a separate AST node); the result fits
/// in `i128` for any Phase-1-supported width up to `u64`.
fn parse_unsigned_int_literal(raw: &str) -> Option<i128> {
    let s = raw.replace('_', "");
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i128::from_str_radix(rest, 16).ok()
    } else if let Some(rest) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        i128::from_str_radix(rest, 8).ok()
    } else if let Some(rest) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        i128::from_str_radix(rest, 2).ok()
    } else {
        s.parse::<i128>().ok()
    }
}

/// Whether a non-negative integer literal value fits in `ty`'s range.
/// `ISize`/`USize` use 64-bit bounds (Phase 1 only targets 64-bit
/// platforms; revisit if a 32-bit target ships).
fn value_fits_int(v: i128, ty: IntTy) -> bool {
    let (min, max): (i128, i128) = match ty {
        IntTy::I8 => (i8::MIN as i128, i8::MAX as i128),
        IntTy::U8 => (0, u8::MAX as i128),
        IntTy::I16 => (i16::MIN as i128, i16::MAX as i128),
        IntTy::U16 => (0, u16::MAX as i128),
        IntTy::I32 => (i32::MIN as i128, i32::MAX as i128),
        IntTy::U32 => (0, u32::MAX as i128),
        IntTy::I64 | IntTy::ISize => (i64::MIN as i128, i64::MAX as i128),
        IntTy::U64 | IntTy::USize => (0, u64::MAX as i128),
    };
    v >= min && v <= max
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
        Expr::Cast(c) => synth_cast(c, cx),
        Expr::Match(m) => synth_match(m, cx),
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

fn synth_literal(l: LiteralExpr<'_>, cx: &mut Cx<'_, '_, '_, '_>) -> Ty {
    match l.token_kind() {
        // Phase 1 literal-to-type mapping. Fancier inference (e.g.
        // ComptimeInt fitting any integer type) is a Phase 2 concern.
        Some(SyntaxKind::IntLit) => Ty::Int(IntTy::I32),
        Some(SyntaxKind::FloatLit) => Ty::Float(FloatTy::F64),
        Some(SyntaxKind::KwTrue) | Some(SyntaxKind::KwFalse) => Ty::Bool,
        Some(SyntaxKind::KwNil) => {
            // `nil` requires an `?T` context — `try_narrow_literal`
            // adopts the expected Optional and never reaches here.
            // If we're in synth mode (no context) or the expected
            // type wasn't an Optional, diagnose.
            cx.diags.push(Diagnostic::error(
                ec::TYPE_MISMATCH,
                Label::new(l.syntax().span, ""),
                "`nil` literal requires an optional context (`?T`); none was inferred here",
            ));
            Ty::Error
        }
        Some(SyntaxKind::RuneLit) => Ty::Rune,
        Some(SyntaxKind::ByteCharLit) => Ty::Int(IntTy::U8),
        Some(SyntaxKind::StringLit | SyntaxKind::RawStringLit) => Ty::Slice(IntTy::U8),
        Some(SyntaxKind::CStringLit) => {
            // C.2: `c"..."` synthesises as `[*:0]u8`. The
            // `[*:0]T → *T` coercion in `ty_assignable` keeps the
            // C.1 tracer's `puts(c"hi")` shape working — the literal's
            // sentinel-typed value flows into `*u8` arg slots without
            // an explicit cast — while preserving the type-level
            // distinction the spec asks for.
            Ty::SentinelPtr {
                elem: IntTy::U8,
                sentinel: 0,
            }
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
    if matches!(b.op_kind(), Some(SyntaxKind::QuestionQ)) {
        return synth_coalesce(b, cx);
    }
    // Narrow free numeric literals to the other side's type when one
    // side has a concrete numeric type and the other is just a bare
    // `IntLit` / `FloatLit`. Without this, expressions like `n < 2`
    // (where `n: i64`) reject because the literal `2` synth'd as i32.
    // The asymmetric case dominates — both-literal and both-typed
    // cases fall back to the plain synth path on each side.
    let (lhs, rhs) = synth_binop_operands(b, cx);
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

/// Synthesise both operands of a binary expression with a one-shot
/// bidirectional rule: if exactly one operand is a bare numeric
/// literal and the other has a concrete int/float type, narrow the
/// literal side to match. Both-literal and both-typed cases fall back
/// to the plain synthesise-each-side path.
fn synth_binop_operands<'a>(b: BinaryExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> (Ty, Ty) {
    let (Some(l_expr), Some(r_expr)) = (b.lhs(), b.rhs()) else {
        return (
            b.lhs().map(|e| synth_expr(e, cx)).unwrap_or(Ty::Error),
            b.rhs().map(|e| synth_expr(e, cx)).unwrap_or(Ty::Error),
        );
    };
    let l_lit = is_free_numeric_literal(l_expr);
    let r_lit = is_free_numeric_literal(r_expr);
    match (l_lit, r_lit) {
        (true, false) => {
            let r_ty = synth_expr(r_expr, cx);
            let l_ty = if matches!(r_ty, Ty::Int(_) | Ty::Float(_)) {
                check_expr(l_expr, r_ty, cx)
            } else {
                synth_expr(l_expr, cx)
            };
            (l_ty, r_ty)
        }
        (false, true) => {
            let l_ty = synth_expr(l_expr, cx);
            let r_ty = if matches!(l_ty, Ty::Int(_) | Ty::Float(_)) {
                check_expr(r_expr, l_ty, cx)
            } else {
                synth_expr(r_expr, cx)
            };
            (l_ty, r_ty)
        }
        _ => (synth_expr(l_expr, cx), synth_expr(r_expr, cx)),
    }
}

/// Whether `e` is a bare integer or float literal — possibly wrapped
/// in parentheses, or prefixed with a unary minus. Used by
/// [`synth_binop_operands`] to recognise the narrowable side; the
/// minus arm covers shapes like `if x == -100` so the literal narrows
/// against `x`'s type.
fn is_free_numeric_literal(e: Expr<'_>) -> bool {
    match e {
        Expr::Literal(l) => matches!(
            l.token_kind(),
            Some(SyntaxKind::IntLit | SyntaxKind::FloatLit)
        ),
        Expr::Paren(p) => p.inner().is_some_and(is_free_numeric_literal),
        Expr::Unary(u) => {
            matches!(u.op_kind(), Some(SyntaxKind::Minus))
                && u.operand().is_some_and(|inner| {
                    matches!(
                        inner,
                        Expr::Literal(l)
                            if matches!(
                                l.token_kind(),
                                Some(SyntaxKind::IntLit | SyntaxKind::FloatLit)
                            )
                    )
                })
        }
        _ => false,
    }
}

/// `x = expr` where `x` is a path to a local. The Phase-1 model
/// treats every let-bound local as mutable; `let` vs `var` is a
/// borrow-checker concern and lands in Phase 3.
/// Phase 2 increment O.1: type-check a `lhs ?? rhs` coalesce
/// expression. The LHS must be a `Ty::Optional`; the RHS is checked
/// against the unwrapped inner type; the result type is the inner
/// type. Bypasses `synth_binop_operands` because the operands are
/// asymmetric (one is an Optional aggregate, the other a primitive).
fn synth_coalesce<'a>(b: BinaryExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    let lhs_ty = b.lhs().map(|e| synth_expr(e, cx)).unwrap_or(Ty::Error);
    let inner = match lhs_ty {
        Ty::Optional(oi) => oi.to_ty(),
        Ty::Error => Ty::Error,
        other => {
            cx.diags.push(Diagnostic::error(
                ec::BAD_OPERAND,
                Label::new(b.syntax().span, ""),
                format!("`??` requires an optional left-hand side, found `{other}`"),
            ));
            Ty::Error
        }
    };
    if let Some(rhs) = b.rhs() {
        check_expr(rhs, inner, cx);
    }
    inner
}

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

/// Type-check `expr as Type`. Phase 1 increments A.1 / A.2 cover
/// numeric casts: any int↔int, any int↔float, and any float↔float
/// pair. Other operands (bool, class, slice, pointer) reject with
/// `UNSUPPORTED_CONSTRUCT`. Narrowing int casts truncate silently
/// (no compile-time bounds check); float→int casts saturate to the
/// destination's range and map NaN to 0 (matches Rust ≥ 1.45 / Zig
/// `@intFromFloat` semantics — the user opted in by writing `as`).
fn synth_cast<'a>(c: CastExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    let src_ty = match c.expr() {
        Some(e) => synth_expr(e, cx),
        None => Ty::Error,
    };
    let dst_ty = match c.target_ty() {
        Some(t) => resolve_type(t, cx.tm.resolved, cx.sm, cx.diags),
        None => Ty::Error,
    };
    if matches!(src_ty, Ty::Error) || matches!(dst_ty, Ty::Error) {
        return Ty::Error;
    }
    match (src_ty, dst_ty) {
        // Numeric ↔ numeric: the cross-product of `Int` and `Float`.
        // Codegen picks the exact Cranelift op from operand widths
        // and signedness (see `arsenal_mir::CastKind`).
        (Ty::Int(_) | Ty::Float(_), Ty::Int(_) | Ty::Float(_)) => dst_ty,
        _ => {
            cx.diags.push(Diagnostic::error(
                ec::UNSUPPORTED_CONSTRUCT,
                Label::new(c.syntax().span, ""),
                format!(
                    "Phase 1 only supports numeric `as` casts (int ↔ int, \
                     int ↔ float, float ↔ float); `{src_ty}` to `{dst_ty}` \
                     is not yet supported"
                ),
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

/// Phase 2 increment M.1: type-check a `match` expression. Synth the
/// scrutinee, validate each arm's pattern against the scrutinee type,
/// unify the arm body types into a single result type. Exhaustiveness
/// rule (Phase-2 minimum): a `_` arm is required for any non-bool
/// scrutinee — the integer domain is too large to enumerate, and the
/// bare `true`/`false` patterns are M.2's concern. Without a wildcard
/// the match diagnoses with UNSUPPORTED_CONSTRUCT.
fn synth_match<'a>(m: MatchExpr<'a>, cx: &mut Cx<'a, '_, '_, '_>) -> Ty {
    let scrutinee_ty = match m.scrutinee() {
        Some(s) => synth_expr(s, cx),
        None => return Ty::Error,
    };
    let arms_node = match m.arms() {
        Some(a) => a,
        None => return Ty::Error,
    };
    let mut result_ty: Option<Ty> = None;
    let mut has_wildcard = false;
    let mut has_true = false;
    let mut has_false = false;
    let mut arm_count: u32 = 0;
    for arm in arms_node.arms() {
        arm_count += 1;
        if let Some(pat) = arm.pattern() {
            check_match_pattern(pat, scrutinee_ty, cx);
            match pat {
                Pattern::Wildcard(_) => has_wildcard = true,
                // For Ty::Bool scrutinees, track whether `true` and
                // `false` patterns are both present so the
                // exhaustiveness rule below can accept the wildcard-
                // free form. The `if scrutinee_ty == Ty::Bool` guard
                // is the easy way to combine the type check with the
                // pattern shape match without nesting `if let`s.
                Pattern::Literal(lp) if scrutinee_ty == Ty::Bool => {
                    if let Some(b) = lp.value().and_then(bool_literal_value) {
                        if b {
                            has_true = true;
                        } else {
                            has_false = true;
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(body) = arm.body() {
            let body_ty = match result_ty {
                None => synth_expr(body, cx),
                Some(rt) => check_expr(body, rt, cx),
            };
            if result_ty.is_none() && body_ty != Ty::Error {
                result_ty = Some(body_ty);
            }
        }
    }
    if arm_count == 0 && scrutinee_ty != Ty::Error {
        cx.diags.push(Diagnostic::error(
            ec::UNSUPPORTED_CONSTRUCT,
            Label::new(m.syntax().span, ""),
            "`match` requires at least one arm",
        ));
    }
    let bool_exhaustive = scrutinee_ty == Ty::Bool && has_true && has_false;
    if !has_wildcard && !bool_exhaustive && scrutinee_ty != Ty::Error && arm_count > 0 {
        cx.diags.push(Diagnostic::error(
            ec::UNSUPPORTED_CONSTRUCT,
            Label::new(m.syntax().span, ""),
            format!(
                "match on `{scrutinee_ty}` is not exhaustive; Phase 2 requires either a `_` arm or (for `bool`) both `true` and `false` arms"
            ),
        ));
    }
    result_ty.unwrap_or(Ty::Error)
}

/// Recognise a bare `nil` literal expression (Phase 2 increment O.2).
/// Used by `check_match_pattern`'s Optional-scrutinee arm. Recurses
/// through parens for symmetry with `bool_literal_value`.
fn is_nil_literal(expr: Expr<'_>) -> bool {
    match expr {
        Expr::Literal(l) => matches!(l.token_kind(), Some(SyntaxKind::KwNil)),
        Expr::Paren(p) => p.inner().is_some_and(is_nil_literal),
        _ => false,
    }
}

/// Recognise a `true`/`false` literal expression. Used by `synth_match`'s
/// bool-exhaustiveness check. Returns `None` for anything else (other
/// literal kinds, parens, paths, …).
fn bool_literal_value(expr: Expr<'_>) -> Option<bool> {
    let lit = match expr {
        Expr::Literal(l) => l,
        Expr::Paren(p) => return p.inner().and_then(bool_literal_value),
        _ => return None,
    };
    match lit.token_kind()? {
        SyntaxKind::KwTrue => Some(true),
        SyntaxKind::KwFalse => Some(false),
        _ => None,
    }
}

/// Validate one match-arm pattern against the scrutinee type.
/// Phase 2 M.1 accepts wildcards everywhere and integer-typed literal
/// patterns when the scrutinee is `Ty::Int(_)`. Identifier patterns
/// (binding) and richer pattern shapes diagnose as
/// UNSUPPORTED_CONSTRUCT.
fn check_match_pattern<'a>(pat: Pattern<'a>, scrut_ty: Ty, cx: &mut Cx<'a, '_, '_, '_>) {
    match pat {
        Pattern::Wildcard(_) => {}
        Pattern::Ident(ip) => {
            cx.diags.push(Diagnostic::error(
                ec::UNSUPPORTED_CONSTRUCT,
                Label::new(ip.syntax().span, ""),
                "identifier patterns in `match` arms aren't supported yet (Phase 2 M.1 accepts only `_` and integer literals)",
            ));
        }
        Pattern::Literal(lp) => {
            let Some(value) = lp.value() else {
                return;
            };
            // Phase 2 O.2: `nil` pattern against an `?T` scrutinee.
            // Other literal kinds against `?T` reject (the user must
            // wildcard-match the some side; binding patterns ride a
            // later sub-bundle).
            if matches!(scrut_ty, Ty::Optional(_)) {
                if is_nil_literal(value) {
                    cx.tm.expr_types.insert(NodePtr(value.syntax()), scrut_ty);
                    return;
                }
                cx.diags.push(Diagnostic::error(
                    ec::TYPE_MISMATCH,
                    Label::new(lp.syntax().span, ""),
                    format!(
                        "only `nil` can match a `{scrut_ty}` scrutinee in Phase 2 (use `_` to match the some side)"
                    ),
                ));
                return;
            }
            // `nil` outside an `?T` scrutinee makes no sense as a
            // pattern; surface a TYPE_MISMATCH explicitly here so the
            // diagnostic doesn't bubble through `synth_literal`'s
            // "requires optional context" message in match-arm
            // position.
            if is_nil_literal(value) && scrut_ty != Ty::Error {
                cx.diags.push(Diagnostic::error(
                    ec::TYPE_MISMATCH,
                    Label::new(lp.syntax().span, ""),
                    format!(
                        "`nil` pattern can't match scrutinee of type `{scrut_ty}` (only `?T` scrutinees accept `nil` patterns)"
                    ),
                ));
                return;
            }
            if matches!(scrut_ty, Ty::Int(_)) {
                // Bidirectional narrowing handles bare `IntLit` and
                // `Unary(Minus, IntLit)` shapes against the scrutinee
                // width — same path used at `let n: i64 = -100;`.
                check_expr(value, scrut_ty, cx);
            } else if scrut_ty == Ty::Bool {
                // Phase 2 M.2: `true` / `false` patterns against a
                // bool scrutinee. `check_expr(value, Ty::Bool, ...)`
                // synthesises the literal as bool and diagnoses any
                // other shape.
                check_expr(value, scrut_ty, cx);
            } else if scrut_ty != Ty::Error {
                cx.diags.push(Diagnostic::error(
                    ec::TYPE_MISMATCH,
                    Label::new(lp.syntax().span, ""),
                    format!(
                        "literal pattern can't match scrutinee of type `{scrut_ty}` in Phase 2"
                    ),
                ));
            }
        }
        Pattern::Range(rp) => {
            // Phase 2 M.3: only integer scrutinees take ranges. Both
            // bounds re-use the bidirectional-narrowing path.
            if matches!(scrut_ty, Ty::Int(_)) {
                if let Some(lo) = rp.lo() {
                    check_expr(lo, scrut_ty, cx);
                }
                if let Some(hi) = rp.hi() {
                    check_expr(hi, scrut_ty, cx);
                }
            } else if scrut_ty != Ty::Error {
                cx.diags.push(Diagnostic::error(
                    ec::TYPE_MISMATCH,
                    Label::new(rp.syntax().span, ""),
                    format!(
                        "range pattern can't match scrutinee of type `{scrut_ty}` (Phase 2 only types ranges over integers)"
                    ),
                ));
            }
        }
        Pattern::Or(op) => {
            // Each alternative recurses through this same routine.
            // M.3 keeps top-level or-patterns only; a sub-pattern
            // alternation (e.g. `Some(x | y)`) doesn't reach here
            // because there's no parser path to it.
            for alt in op.alternatives() {
                check_match_pattern(alt, scrut_ty, cx);
            }
        }
        Pattern::Stub(n) | Pattern::Error(n) => {
            cx.diags.push(Diagnostic::error(
                ec::UNSUPPORTED_CONSTRUCT,
                Label::new(n.span, ""),
                "this pattern shape isn't supported yet",
            ));
        }
    }
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
    if actual == expected || actual == Ty::Error || expected == Ty::Error {
        return true;
    }
    // Phase 2 C.2: `[*:S]T` decays to `*T` at any assignable position
    // (extern call args, `let s: *u8 = c"hi";`, return-into-`*u8`).
    // The decay is value-level identity — both lower to the same
    // pointer at MIR/codegen — so accepting the implicit form here
    // costs nothing at runtime and avoids forcing every c-string user
    // to write an `as *u8` that doesn't yet parse.
    if let (Ty::SentinelPtr { elem: e1, .. }, Ty::Ptr(e2)) = (actual, expected) {
        if e1 == e2 {
            return true;
        }
    }
    // Phase 2 O.1: `T → ?T` coercion. The inner type must match the
    // Optional's inner exactly; this is the value-level wrap (tag = 1
    // + payload = T) materialised at MIR-lowering time. Reverse
    // direction (`?T → T`) is rejected — the user must unwrap via
    // `??` (or later `!` / `match`).
    if let Ty::Optional(inner) = expected {
        if actual == inner.to_ty() {
            return true;
        }
    }
    false
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
        Expr::Cast(x) => x.syntax().span,
        Expr::Match(x) => x.syntax().span,
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
    fn missing_return_type_defaults_to_u0() {
        // No `-> T` annotation — the helper falls through without a
        // value, which is well-typed against the implicit `u0` return.
        let src = "fn helper(x: i32) { let y: i32 = x; } fn main() -> i32 { helper(1); return 0; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn missing_return_type_rejects_value_return() {
        // No `-> T` annotation defaults to `u0`; returning a non-unit
        // value must still diagnose against the inferred `u0`.
        assert!(check("fn helper() { return 1; } fn main() -> i32 { return 0; }") >= 1);
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

    // ─── Phase 2 increment C.1: c-string literals ─────────────────────────

    #[test]
    fn cstring_literal_passes_to_extern_ptr_param() {
        // `c"hi"` synths to `*u8`; flows into an extern param of type
        // `*u8` without diagnostic.
        let src = "extern fn puts(s: *u8) -> i32; fn main() -> i32 { puts(c\"hi\"); return 0; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn cstring_literal_does_not_coerce_to_slice() {
        // C.2 keeps `c"..."` strictly typed as `[*:0]u8`; assigning
        // it to a `[]u8` binding must diagnose. The C.2 coercion only
        // covers `[*:S]T → *T`, never `[*:S]T → []T`.
        let src = "fn main() -> i32 { let s: []u8 = c\"hi\"; return 0; }";
        assert!(check(src) >= 1);
    }

    // ─── Phase 2 increment C.2: `[*:0]u8` sentinel pointer type ───────────

    #[test]
    fn cstring_synthesises_as_sentinel_ptr() {
        // After C.2, `c"..."` types as `[*:0]u8`; a `let s: [*:0]u8`
        // annotation accepts it without coercion.
        let src = "fn main() -> i32 { let s: [*:0]u8 = c\"hi\"; return 0; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn sentinel_ptr_accepts_only_sentinel_zero_u8() {
        // Phase 2 only realises `[*:0]u8`. Other element types
        // (e.g. `[*:0]u32`) and other sentinels (`[*:1]u8`) reject
        // with UNSUPPORTED_CONSTRUCT.
        assert!(check("fn main() -> i32 { let s: [*:0]u32; return 0; }") >= 1);
        assert!(check("fn main() -> i32 { let s: [*:1]u8; return 0; }") >= 1);
    }

    #[test]
    fn sentinel_ptr_decays_to_raw_ptr_at_extern_call() {
        // The C.1 tracer's coercion path: `c"..."` is now
        // `[*:0]u8` but flows into `extern fn puts(*u8)` cleanly.
        let src = "extern fn puts(s: *u8) -> i32; fn main() -> i32 { puts(c\"hi\"); return 0; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn sentinel_ptr_works_as_non_extern_fn_param() {
        // Helper-fn fixtures need `[*:0]u8` in non-extern signatures
        // (cleanup #1's `-> u0` default also exercised here).
        let src = "extern fn puts(s: *u8) -> i32;\n\
                   fn greet(s: [*:0]u8) { puts(s); }\n\
                   fn main() -> i32 { greet(c\"hi\"); return 0; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn sentinel_ptr_displays_with_zig_syntax() {
        let ty = Ty::SentinelPtr {
            elem: IntTy::U8,
            sentinel: 0,
        };
        assert_eq!(format!("{}", ty), "[*:0]u8");
    }

    // ─── Phase 2 increment M.1: match (literal int + wildcard) ────────────

    #[test]
    fn match_int_literal_with_wildcard_typechecks() {
        let src = "fn classify(x: i32) -> i32 {\n\
                       return match x { 0 => 100, 1 => 200, _ => 7 };\n\
                   }\n\
                   fn main() -> i32 { return classify(1); }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn match_without_wildcard_diagnoses() {
        // No `_` arm and integer scrutinee — Phase 2 M.1 requires
        // wildcard for non-bool exhaustiveness.
        let src = "fn classify(x: i32) -> i32 { return match x { 0 => 1, 1 => 2 }; }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn match_negative_literal_pattern_narrows_to_scrutinee_width() {
        // `-3` inside a match arm should narrow to `i32` (the
        // scrutinee width) just like `let n: i32 = -3;` does.
        let src = "fn signed(x: i32) -> i32 { return match x { -3 => 1, _ => 0 }; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn match_literal_pattern_must_match_scrutinee_kind() {
        // Class-typed scrutinee + integer-literal pattern: rejects.
        let src = "class C { x: i32 }\n\
                   fn pick(c: C) -> i32 { return match c { 0 => 1, _ => 2 }; }\n\
                   fn main() -> i32 { return 0; }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn match_arm_bodies_must_unify() {
        // Two arms with mismatched body types — second arm's body
        // should diagnose against the first arm's `i32` type.
        let src = "fn f(x: i32) -> i32 { return match x { 0 => 1, _ => true }; }";
        assert!(check(src) >= 1);
    }

    // ─── Phase 2 increment M.2: bool match + statement-position match ─────

    #[test]
    fn match_bool_with_both_arms_is_exhaustive_without_wildcard() {
        // Bool scrutinee + true + false arms — no `_` required.
        let src = "fn pick(b: bool) -> i32 { return match b { true => 1, false => 0 }; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn match_bool_missing_one_arm_diagnoses() {
        // Only `true`, no `false`, no `_` — exhaustiveness fires.
        let src = "fn pick(b: bool) -> i32 { return match b { true => 1 }; }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn match_bool_with_wildcard_still_works() {
        let src = "fn pick(b: bool) -> i32 { return match b { true => 1, _ => 0 }; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn match_at_statement_position_yields_unit() {
        // Stmt-position match: arm bodies are unit-shaped (here, a
        // discarded extern call result). The match expression itself
        // is consumed by `ExprStmt` and produces no value.
        let src = "extern fn putchar(c: i32) -> i32;\n\
                   fn shout(n: i32) {\n\
                       match n { 0 => putchar(65), _ => putchar(63) };\n\
                   }\n\
                   fn main() -> i32 { shout(0); return 0; }";
        assert_eq!(check(src), 0);
    }

    // ─── Phase 2 increment M.3: range + or-patterns ───────────────────────

    #[test]
    fn match_or_pattern_typechecks() {
        let src = "fn classify(x: i32) -> i32 {\n\
                       return match x { 1 | 2 | 3 => 10, _ => 0 };\n\
                   }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn match_inclusive_range_pattern_typechecks() {
        let src = "fn bucket(x: i32) -> i32 {\n\
                       return match x { 0..=9 => 1, _ => 0 };\n\
                   }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn match_range_pattern_rejects_non_int_scrutinee() {
        // Range patterns over a bool scrutinee should diagnose.
        let src = "fn f(b: bool) -> i32 { return match b { 0..=9 => 1, _ => 0 }; }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn match_or_pattern_alternative_types_must_match_scrutinee() {
        // One of the alternatives is type-mismatched against the
        // scrutinee.
        let src = "fn f(b: bool) -> i32 { return match b { true | 5 => 1, false => 0 }; }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn match_range_pattern_bounds_narrow_to_scrutinee_width() {
        // i64 scrutinee + i32-shaped literal bounds: the bidirectional
        // narrowing path widens both bounds to i64.
        let src = "fn f(x: i64) -> i32 { return match x { 100..=200 => 1, _ => 0 }; }";
        assert_eq!(check(src), 0);
    }

    // ─── Phase 2 increment O.1: ?T optional + nil + ?? ─────────────────────

    #[test]
    fn optional_let_init_with_value_typechecks() {
        // Bare `T` coerces to `?T` at let-init position.
        let src = "fn main() -> i32 { let x: ?i32 = 7; return 0; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn optional_let_init_with_nil_typechecks() {
        // `nil` adopts `?T` from the let annotation.
        let src = "fn main() -> i32 { let x: ?i32 = nil; return 0; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn nil_outside_optional_context_diagnoses() {
        // Without a `?T` context, `nil` synthesises as Ty::Error and
        // the surrounding check fires.
        let src = "fn main() -> i32 { let x: i32 = nil; return x; }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn coalesce_extracts_inner_type() {
        // `(x ?? 0): i32` when `x: ?i32`.
        let src = "fn main() -> i32 { let x: ?i32 = 7; return x ?? 0; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn coalesce_rejects_non_optional_lhs() {
        // `??` requires `?T` LHS.
        let src = "fn main() -> i32 { let x: i32 = 7; return x ?? 0; }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn coalesce_default_must_match_inner_type() {
        // `?i32 ?? bool` rejects.
        let src = "fn main() -> i32 { let x: ?i32 = nil; return x ?? true; }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn optional_ty_displays_with_question_prefix() {
        let ty = Ty::Optional(OptInner::Int(IntTy::I32));
        assert_eq!(format!("{}", ty), "?i32");
        let bool_opt = Ty::Optional(OptInner::Bool);
        assert_eq!(format!("{}", bool_opt), "?bool");
    }

    #[test]
    fn optional_does_not_coerce_to_inner_implicitly() {
        // The reverse direction (`?T → T`) is rejected; the user must
        // unwrap via `??` (or later `!` / `match`).
        let src = "fn main() -> i32 { let x: ?i32 = 7; let y: i32 = x; return y; }";
        assert!(check(src) >= 1);
    }

    // ─── Phase 2 increment O.2: ?T match patterns + nil arm ────────────────

    #[test]
    fn match_optional_with_nil_and_wildcard_typechecks() {
        let src = "fn main() -> i32 {\n\
                       let opt: ?i32 = nil;\n\
                       return match opt { nil => 1, _ => 2 };\n\
                   }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn match_optional_without_wildcard_diagnoses() {
        // Optional scrutinee with only `nil` arm doesn't cover the
        // some side; exhaustiveness rule fires.
        let src = "fn main() -> i32 {\n\
                       let opt: ?i32 = nil;\n\
                       return match opt { nil => 1 };\n\
                   }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn match_optional_rejects_non_nil_literal_pattern() {
        // `?i32` scrutinee + integer literal pattern: Phase 2 doesn't
        // unwrap implicitly; user must wildcard the some side.
        let src = "fn main() -> i32 {\n\
                       let opt: ?i32 = 7;\n\
                       return match opt { 7 => 1, _ => 2 };\n\
                   }";
        assert!(check(src) >= 1);
    }

    #[test]
    fn nil_pattern_rejects_non_optional_scrutinee() {
        // `nil` pattern only makes sense against `?T` scrutinees.
        let src = "fn main() -> i32 { return match 5 { nil => 1, _ => 0 }; }";
        assert!(check(src) >= 1);
    }

    /// Pre-A.4 a `[]u8` parameter rejected with UNSUPPORTED_CONSTRUCT.
    /// A.4 enables it via the by-pointer ABI; this test now pins the
    /// inverse — that the previously-rejected shape compiles cleanly.
    #[test]
    fn slice_typed_param_now_typechecks() {
        assert_eq!(check("fn g(s: []u8) -> i32 { return 0; }"), 0);
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

    /// Regression: integer literals in `let` initializers were stuck
    /// at `i32` and rejected against any other typed binding —
    /// `let n: i64 = 5;` errored "expected i64, found i32". Fix wires
    /// bidirectional narrowing through `check_expr` so the literal
    /// adopts the expected width when the value fits.
    #[test]
    fn int_literal_narrows_in_let_init() {
        assert_eq!(check("fn f() -> i32 { let n: i64 = 5; return 0; }"), 0);
        assert_eq!(check("fn f() -> i32 { let n: u8 = 200; return 0; }"), 0);
        assert_eq!(check("fn f() -> i32 { let n: usize = 1024; return 0; }"), 0);
    }

    /// Out-of-range narrowing is still a type error; the diagnostic
    /// names the requested type so the user knows what's wrong.
    #[test]
    fn int_literal_narrowing_out_of_range_diagnoses() {
        assert!(check("fn f() -> i32 { let n: u8 = 256; return 0; }") >= 1);
        assert!(check("fn f() -> i32 { let n: i8 = 200; return 0; }") >= 1);
    }

    /// Narrowing also fires inside binary operators when one side has
    /// a known type and the other is a free literal — needed for
    /// shapes like `n < 2`, `r == 3628800`, `n - 1` where `n: i64`.
    #[test]
    fn int_literal_narrows_across_binop_with_typed_side() {
        let src = "fn fact(n: i64) -> i64 {
                if n < 2 { return 1; }
                return n * fact(n - 1);
            }
            fn main() -> i32 {
                let r: i64 = fact(10);
                if r == 3628800 { return 42; }
                return 0;
            }";
        assert_eq!(check(src), 0);
    }

    /// Float literals should narrow analogously: `let x: f32 = 1.5;`
    /// previously errored "expected f32, found f64".
    #[test]
    fn float_literal_narrows_in_let_init() {
        assert_eq!(check("fn f() -> i32 { let x: f32 = 1.5; return 0; }"), 0);
    }

    /// Call argument narrowing: positional args at function calls
    /// already used `check_expr`; the literal-narrowing path through
    /// `check_expr` makes wide-int call sites work without per-call
    /// casts.
    #[test]
    fn int_literal_narrows_in_call_arg() {
        let src = "fn g(n: i64) -> i64 { return n; }
                   fn f() -> i32 { let r: i64 = g(42); return 0; }";
        assert_eq!(check(src), 0);
    }

    /// Negated literals (`Unary(Minus, IntLit)`) narrow alongside bare
    /// literals. Without this extension, shapes like `let n: i64 = -1;`
    /// or `if x == -100` (with `x: i64`) reject because the unary
    /// expression keeps its operand's `i32` synth type.
    #[test]
    fn negated_literal_narrows() {
        assert_eq!(check("fn f() -> i32 { let n: i64 = -1; return 0; }"), 0);
        assert_eq!(
            check(
                "fn f(x: i64) -> i32 {
                    if x == -100 { return 1; }
                    return 0;
                }"
            ),
            0
        );
        // Out-of-range still diagnoses; the message uses the negated form.
        assert!(check("fn f() -> i32 { let n: i8 = -200; return 0; }") >= 1);
    }

    /// Paren-wrapped literals narrow through the parens, so users can
    /// write `(-7)` or `(42)` interchangeably with the bare form.
    #[test]
    fn paren_wrapped_literal_narrows() {
        assert_eq!(check("fn f() -> i32 { let n: i64 = (42); return 0; }"), 0);
        assert_eq!(check("fn f() -> i32 { let n: i64 = (-1); return 0; }"), 0);
    }

    // ─── Phase 1 increment A.1: `as` cast (int↔int) ──────────────────────

    #[test]
    fn cast_int_widen_typechecks_clean() {
        assert_eq!(check("fn f(x: i32) -> i64 { return x as i64; }"), 0);
        assert_eq!(check("fn f(x: u8) -> u32 { return x as u32; }"), 0);
    }

    #[test]
    fn cast_int_narrow_typechecks_clean() {
        // No bounds check; `as` always succeeds at typeck for int↔int.
        assert_eq!(check("fn f(x: i64) -> i32 { return x as i32; }"), 0);
    }

    #[test]
    fn cast_signedness_reinterpret_typechecks_clean() {
        assert_eq!(check("fn f(x: i32) -> u32 { return x as u32; }"), 0);
        assert_eq!(check("fn f(x: u8) -> i8 { return x as i8; }"), 0);
    }

    #[test]
    fn cast_int_to_bool_diagnoses() {
        // A.1 explicitly rejects non-int-to-int casts; the diagnostic
        // text mentions the unsupported pair so users know `as bool` is
        // not just absent but disallowed.
        assert!(check("fn f(x: i32) -> bool { return x as bool; }") >= 1);
    }

    #[test]
    fn cast_bool_to_int_diagnoses() {
        assert!(check("fn f() -> i32 { return true as i32; }") >= 1);
    }

    // ─── Phase 1 increment A.2: `as` cast float bridge ────────────────────

    #[test]
    fn cast_int_to_float_typechecks_clean() {
        assert_eq!(check("fn f(x: i32) -> f64 { return x as f64; }"), 0);
        assert_eq!(check("fn f(x: u8) -> f32 { return x as f32; }"), 0);
    }

    #[test]
    fn cast_float_to_int_typechecks_clean() {
        assert_eq!(check("fn f(x: f64) -> i32 { return x as i32; }"), 0);
        assert_eq!(check("fn f(x: f32) -> u64 { return x as u64; }"), 0);
    }

    #[test]
    fn cast_float_to_float_typechecks_clean() {
        assert_eq!(check("fn f(x: f32) -> f64 { return x as f64; }"), 0);
        assert_eq!(check("fn f(x: f64) -> f32 { return x as f32; }"), 0);
        // Same-width identity is also legal.
        assert_eq!(check("fn f(x: f32) -> f32 { return x as f32; }"), 0);
    }

    /// Non-numeric operands are still rejected — A.2 only opens up
    /// the float arms of the numeric matrix.
    #[test]
    fn cast_bool_to_float_still_diagnoses() {
        assert!(check("fn f() -> f64 { return true as f64; }") >= 1);
    }

    // ─── Phase 1 increment A.3: class-by-pointer ABI ──────────────────────

    #[test]
    fn class_typed_param_typechecks_clean() {
        // Pre-A.3 this rejected with UNSUPPORTED_CONSTRUCT.
        let src = "class C { x: i32 }\n\
                   fn f(c: C) -> i32 { return c.x; }\n\
                   fn main() -> i32 { let c = C { .x = 0 }; return f(c); }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn class_typed_return_typechecks_clean() {
        // Pre-A.3 this rejected with UNSUPPORTED_CONSTRUCT.
        let src = "class C { x: i32 }\n\
                   fn make() -> C { return C { .x = 0 }; }\n\
                   fn main() -> i32 { let c = make(); return c.x; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn class_param_and_return_compose() {
        let src = "class C { x: i32 }\n\
                   fn doubled(c: C) -> C { return C { .x = c.x + c.x }; }\n\
                   fn main() -> i32 { let c = C { .x = 3 }; let d = doubled(c); return d.x; }";
        assert_eq!(check(src), 0);
    }

    // ─── Phase 1 increment A.4: slice-by-pointer ABI ──────────────────────

    #[test]
    fn slice_typed_param_typechecks_clean() {
        // Pre-A.4 this rejected with UNSUPPORTED_CONSTRUCT.
        let src = "fn len_of(s: []u8) -> usize { return s.len; }\n\
                   fn main() -> i32 { let s: []u8 = \"hi\"; let n: usize = len_of(s); return 0; }";
        assert_eq!(check(src), 0);
    }

    #[test]
    fn slice_typed_return_typechecks_clean() {
        // Pre-A.4 this rejected with UNSUPPORTED_CONSTRUCT.
        let src = "fn make() -> []u8 { return \"abc\"; }\n\
                   fn main() -> i32 { let s: []u8 = make(); return s.len as i32; }";
        assert_eq!(check(src), 0);
    }
}
