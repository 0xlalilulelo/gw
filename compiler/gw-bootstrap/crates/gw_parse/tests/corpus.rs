//! Lex+parse conformance corpus.
//!
//! See `docs/architecture.md` Part K.2: per-language test corpus split
//! into `pass/` (programs that must compile cleanly) and `fail/`
//! (programs that must produce specific diagnostics).
//!
//! The Phase-0 slice is `tests/corpus/{pass,fail}/lexparse/` at the
//! repository root.
//!
//! ## Pass tests
//!
//! Each `*.gw` under `pass/lexparse/` must lex+parse with zero
//! error-severity diagnostics. The AST dump is captured as a snapshot
//! via `insta`. To accept new or updated snapshots after intentional
//! changes:
//!
//! ```text
//! cargo insta accept -p gw_parse
//! ```
//!
//! ## Fail tests
//!
//! Each `*.gw` under `fail/lexparse/` is paired with a sibling
//! `*.expected_diagnostics` file. The format is one `EXXXX:line:col`
//! triple per line; blank lines and `//` comments are ignored. The
//! actual list of diagnostics produced by the lexer + parser is
//! compared to the expected list verbatim, in order.

use bumpalo::Bump;
use gw_ast::{dump, FileArena};
use gw_lex::{DiagBag, SourceMap};
use gw_parse::parse;
use std::fs;

fn parse_one(path: &std::path::Path) -> (String, DiagBag, SourceMap, gw_lex::FileId) {
    let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut sm = SourceMap::new();
    let file = sm.add_file(path.display().to_string(), src);
    let bytes = sm.get(file).unwrap().contents.as_bytes();
    let bump = Bump::new();
    let arena = FileArena::new(&bump, file);
    let (root, diags) = parse(file, bytes, &arena);
    let dump_text = dump(root, &sm);
    (dump_text, diags, sm, file)
}

#[test]
fn pass_corpus() {
    insta::glob!(
        "../../../../../tests/corpus/pass/lexparse",
        "*.gw",
        |path| {
            let (dump_text, diags, sm, _) = parse_one(path);
            assert!(
                !diags.has_errors(),
                "{} should lex+parse cleanly; got {} error(s):\n{}",
                path.display(),
                diags.error_count(),
                diags
                    .iter()
                    .map(|d| gw_lex::render_simple(d, &sm))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            insta::assert_snapshot!(dump_text);
        }
    );
}

#[test]
fn fail_corpus() {
    insta::glob!(
        "../../../../../tests/corpus/fail/lexparse",
        "*.gw",
        |path| {
            let (_dump, diags, sm, _) = parse_one(path);
            // Build the actual triple list from the diagnostics.
            let actual: Vec<String> = diags
                .iter()
                .map(|d| {
                    let code = d
                        .code
                        .map(|c| format!("{c}"))
                        .unwrap_or_else(|| "E?".to_string());
                    let (line, col) = sm.line_col(d.primary.span).unwrap_or((0, 0));
                    format!("{code}:{line}:{col}")
                })
                .collect();

            // Read expected list.
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
                "diagnostic list mismatch for {}\nactual:\n  {}\nexpected:\n  {}",
                path.display(),
                actual.join("\n  "),
                expected.join("\n  "),
            );
        }
    );
}
