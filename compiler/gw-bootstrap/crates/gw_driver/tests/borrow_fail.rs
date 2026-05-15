//! Borrow-checker fail-corpus walker (Phase 3 increment B.3).
//!
//! Mirrors `gw_parse/tests/corpus.rs::fail_corpus` for the
//! lex+parse layer: each `tests/corpus/fail/borrow/<name>.gw` has a
//! sibling `<name>.expected_diagnostics` file listing one
//! `EXXXX:line:col` triple per non-blank, non-`//` line. The actual
//! diagnostics produced by running the full pipeline through
//! `gw_borrow::check_program` must match the expected list verbatim,
//! in order.
//!
//! These programs must lex+parse+resolve+type-check cleanly — the
//! diagnostic must come from the borrow checker, not an earlier
//! stage. Test panics if any prior stage emits errors.

use bumpalo::Bump;
use gw_ast::FileArena;
use gw_lex::{DiagBag, SourceMap};
use gw_mir::lower;
use gw_parse::parse;
use gw_resolve::resolve_modules;
use gw_typeck::type_check;
use std::fs;

#[test]
fn borrow_fail_corpus() {
    insta::glob!("../../../../../tests/corpus/fail/borrow", "*.gw", |path| {
        let actual = run_through_borrow(path);

        let expected_path = path.with_extension("expected_diagnostics");
        let expected_text = fs::read_to_string(&expected_path).unwrap_or_else(|e| {
            panic!(
                "missing or unreadable expected file {}: {e}",
                expected_path.display()
            )
        });
        let expected: Vec<String> = expected_text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            .map(str::to_string)
            .collect();

        assert_eq!(
            actual,
            expected,
            "borrow-check diagnostic list mismatch for {}\nactual:\n  {}\nexpected:\n  {}",
            path.display(),
            actual.join("\n  "),
            expected.join("\n  "),
        );
    });
}

fn run_through_borrow(path: &std::path::Path) -> Vec<String> {
    let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut sm = SourceMap::new();
    let file = sm.add_file(path.display().to_string(), src);
    let bytes = sm.get(file).unwrap().contents.as_bytes();
    let bump = Bump::new();
    let arena = FileArena::new(&bump, file);
    let (root, parse_diags) = parse(file, bytes, &arena);
    assert!(
        !parse_diags.has_errors(),
        "{}: parse must succeed before borrow check; got {} error(s)",
        path.display(),
        parse_diags.error_count(),
    );

    let mut diags = DiagBag::new();
    diags.merge(parse_diags);
    let resolved = resolve_modules(root, &[], &sm, &mut diags);
    assert!(
        !diags.has_errors(),
        "{}: resolve must succeed before borrow check; got {} error(s)",
        path.display(),
        diags.error_count(),
    );

    let typed = type_check(&resolved, &sm, &mut diags);
    assert!(
        !diags.has_errors(),
        "{}: typeck must succeed before borrow check; got {} error(s)",
        path.display(),
        diags.error_count(),
    );

    let mir = lower(&typed, &resolved, &sm);
    let borrow_diags = gw_borrow::check_program(&mir);
    borrow_diags
        .iter()
        .map(|d| {
            let code = d
                .code
                .map(|c| format!("{c}"))
                .unwrap_or_else(|| "E?".to_string());
            let (line, col) = sm.line_col(d.primary.span).unwrap_or((0, 0));
            format!("{code}:{line}:{col}")
        })
        .collect()
}

