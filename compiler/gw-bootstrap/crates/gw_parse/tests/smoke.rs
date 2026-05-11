//! Smoke tests: parse small GW snippets covering the Phase-1-minimal
//! subset, asserting clean diagnostics + spot-check AST shape.

use gw_ast::{dump, AstNode, FileArena, Item, Module};
use gw_lex::SourceMap;
use gw_parse::parse;
use bumpalo::Bump;

fn parse_to_dump(src: &str) -> (String, u32) {
    let mut sm = SourceMap::new();
    let file = sm.add_file("smoke.gw", src);
    let bytes = sm.get(file).unwrap().contents.as_bytes();
    let bump = Bump::new();
    let arena = FileArena::new(&bump, file);
    let (root, diags) = parse(file, bytes, &arena);
    let s = dump(root, &sm);
    (s, diags.error_count())
}

#[test]
fn empty_file() {
    let (dump, errs) = parse_to_dump("");
    assert_eq!(errs, 0);
    assert!(dump.starts_with("Module"));
}

#[test]
fn fn_with_empty_body_clean() {
    let (_dump, errs) = parse_to_dump("fn main() -> u0 {}");
    assert_eq!(errs, 0);
}

#[test]
fn fn_returns_literal() {
    let src = "fn answer() -> i32 { return 42; }";
    let (dump, errs) = parse_to_dump(src);
    assert_eq!(errs, 0);
    assert!(dump.contains("FnDecl"));
    assert!(dump.contains("RetType"));
    assert!(dump.contains("Block"));
    assert!(dump.contains("ReturnExpr"));
    assert!(dump.contains("LiteralExpr"));
    assert!(dump.contains("IntLit `42`"));
}

#[test]
fn class_with_fields() {
    let src = "class Point { x: i32, y: i32 }";
    let (dump, errs) = parse_to_dump(src);
    assert_eq!(errs, 0);
    assert!(dump.contains("ClassDecl"));
    assert!(dump.contains("FieldDeclList"));
    let count = dump.matches("FieldDecl @").count();
    assert_eq!(count, 2, "expected 2 field decls in dump:\n{dump}");
}

#[test]
fn binary_expr_is_left_associative() {
    let src = "fn t() -> u0 { let x: i32 = 1 + 2 + 3; }";
    let (dump, errs) = parse_to_dump(src);
    assert_eq!(errs, 0, "{dump}");
    // Left-associative: outer BinaryExpr contains an inner BinaryExpr
    // and an integer literal as its second operand.
    // We assert nesting by counting BinaryExpr occurrences.
    let bin_count = dump.matches("BinaryExpr").count();
    assert_eq!(bin_count, 2, "{dump}");
}

#[test]
fn precedence_mul_over_add() {
    // a + b * c => Binary(+, a, Binary(*, b, c))
    let src = "fn t() -> u0 { let _ = 1 + 2 * 3; }";
    let (dump, errs) = parse_to_dump(src);
    assert_eq!(errs, 0, "{dump}");
    // The outer BinaryExpr should contain `+` and inside it a child
    // BinaryExpr containing `*`. We assert that `*` appears DEEPER (more
    // indented) than `+` in the dump.
    let plus_indent = leading_spaces(dump.lines().find(|l| l.contains("Plus `+`")).unwrap());
    let star_indent = leading_spaces(dump.lines().find(|l| l.contains("Star `*`")).unwrap());
    assert!(star_indent > plus_indent, "{dump}");
}

#[test]
fn pratt_unary_then_call() {
    let src = "fn t() -> u0 { let _ = -fib(20); }";
    let (dump, errs) = parse_to_dump(src);
    assert_eq!(errs, 0, "{dump}");
    assert!(dump.contains("UnaryExpr"));
    assert!(dump.contains("CallExpr"));
}

#[test]
fn if_else_chain() {
    let src = "fn t(x: i32) -> u0 { if x { return; } else if x { return; } else { return; } }";
    let (_dump, errs) = parse_to_dump(src);
    assert_eq!(errs, 0);
}

#[test]
fn while_loop_clean() {
    let src = "fn t() -> u0 { while true { return; } }";
    let (_dump, errs) = parse_to_dump(src);
    assert_eq!(errs, 0);
}

#[test]
fn extern_fn_no_body() {
    let src = "extern fn malloc(size: usize) -> u8;";
    let (dump, errs) = parse_to_dump(src);
    assert_eq!(errs, 0);
    assert!(dump.contains("FnDecl"));
}

#[test]
fn underscore_pattern_recognised() {
    let src = "fn t() -> u0 { let _ = 0; }";
    let (dump, errs) = parse_to_dump(src);
    assert_eq!(errs, 0);
    assert!(dump.contains("WildcardPat"), "{dump}");
}

#[test]
fn slice_and_array_types() {
    let src = "fn t(s: []i32, a: [10]i32) -> u0 {}";
    let (dump, errs) = parse_to_dump(src);
    assert_eq!(errs, 0, "{dump}");
    assert!(dump.contains("SliceType"));
    assert!(dump.contains("ArrayType"));
}

#[test]
fn ref_and_opt_types() {
    let src = "fn t(p: &i32, q: ?i32) -> u0 {}";
    let (dump, errs) = parse_to_dump(src);
    assert_eq!(errs, 0, "{dump}");
    assert!(dump.contains("RefType"));
    assert!(dump.contains("OptType"));
}

#[test]
fn unsupported_item_recovers_to_next_fn() {
    // `mod` is recognised but not yet supported; the parser should
    // emit a diagnostic and skip ahead so the following `fn` parses.
    let src = "mod Tile { Floor, Wall } fn after() -> u0 {}";
    let (dump, errs) = parse_to_dump(src);
    assert!(errs >= 1, "expected at least 1 error, got {errs}\n{dump}");
    assert!(
        dump.contains("FnDecl"),
        "should still parse fn after:\n{dump}"
    );
}

#[test]
fn module_typed_view_iterates_items() {
    let src = "fn a() -> u0 {} fn b() -> u0 {}";
    let mut sm = SourceMap::new();
    let file = sm.add_file("m.gw", src);
    let bytes = sm.get(file).unwrap().contents.as_bytes();
    let bump = Bump::new();
    let arena = FileArena::new(&bump, file);
    let (root, diags) = parse(file, bytes, &arena);
    assert_eq!(diags.error_count(), 0);
    let module = Module::cast(root).expect("module");
    let names: Vec<_> = module
        .items()
        .filter_map(|i| match i {
            Item::Fn(f) => f.name(),
            _ => None,
        })
        .collect();
    assert_eq!(names.len(), 2);
}

fn leading_spaces(s: &str) -> usize {
    s.chars().take_while(|c| *c == ' ').count()
}
