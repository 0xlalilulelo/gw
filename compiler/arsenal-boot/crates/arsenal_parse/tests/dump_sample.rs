//! Parse the same fixture used by the lexer dump test, print the AST
//! dump and the diagnostics. Run with:
//!
//! ```text
//! cargo test -p arsenal_parse --test dump_sample -- --nocapture
//! ```
//!
//! The fixture intentionally exercises many Phase-2+ constructs the
//! Phase-0 parser does not support. The test asserts only that the
//! parser produces *some* CST root (no panics, makes progress) — the
//! diagnostics + AST output document where the parser stopped and how
//! recovery played out.

use arsenal_ast::{dump_with, DumpOpts, FileArena};
use arsenal_lex::{render_simple, SourceMap};
use arsenal_parse::parse;
use bumpalo::Bump;

const FIXTURE: &str = include_str!("../../arsenal_lex/tests/fixtures/sample.gw");

#[test]
fn parse_sample_and_dump() {
    let mut sm = SourceMap::new();
    let file = sm.add_file("sample.gw", FIXTURE);
    let bytes = sm.get(file).unwrap().contents.as_bytes();
    let bump = Bump::new();
    let arena = FileArena::new(&bump, file);
    let (root, diags) = parse(file, bytes, &arena);

    println!();
    println!(
        "=== sample.gw : {} bytes, {} diagnostics ({} errors) ===",
        bytes.len(),
        diags.len(),
        diags.error_count(),
    );
    println!();
    println!("--- diagnostics ---");
    for d in diags.iter() {
        println!("  {}", render_simple(d, &sm));
    }
    println!();
    println!("--- AST (default opts: doc-comments shown, ws/comments elided) ---");
    let s = dump_with(root, &sm, DumpOpts::default());
    println!("{s}");

    // The fixture is broader than Phase 0 supports; we just assert we
    // produced a Module root and didn't crash.
    assert_eq!(root.kind, arsenal_ast::SyntaxKind::Module);
}
