//! Lex the Phase 0 fixture and dump the token stream.
//!
//! Run with `cargo test -p arsenal_lex --test dump_sample -- --nocapture`
//! to see the dump on stdout. The test asserts the fixture lexes without
//! diagnostics; the printout is for human review.

use arsenal_lex::{lex, render_simple, SourceMap, TokenKind};
use std::collections::BTreeMap;

const FIXTURE: &str = include_str!("fixtures/sample.gw");

#[test]
fn lex_sample_clean_and_dump() {
    let mut sm = SourceMap::new();
    let file = sm.add_file("sample.gw", FIXTURE);
    let src = sm
        .get(file)
        .expect("file just inserted")
        .contents
        .as_bytes();
    let (tokens, diags) = lex(file, src);
    let f = sm.get(file).expect("file present");

    // ── Header ─────────────────────────────────────────────────────────
    let trivia = tokens.iter().filter(|t| t.kind.is_trivia()).count();
    let non_trivia = tokens.len() - trivia;
    println!();
    println!(
        "=== {} : {} bytes, {} tokens ({} significant + {} trivia) ===",
        f.name,
        src.len(),
        tokens.len(),
        non_trivia,
        trivia,
    );
    println!("    diagnostics: {}", diags.len());
    println!();

    // ── Histogram by kind ──────────────────────────────────────────────
    let mut hist: BTreeMap<String, u32> = BTreeMap::new();
    for t in &tokens {
        *hist.entry(format!("{:?}", t.kind)).or_default() += 1;
    }
    println!("--- coverage histogram ---");
    for (k, v) in &hist {
        println!("  {k:<18} {v}");
    }
    println!();

    // ── Non-trivia "spine" ─────────────────────────────────────────────
    println!("--- non-trivia token stream ---");
    for (i, t) in tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.kind.is_trivia())
    {
        let (line, col) = f.line_col(t.span.start);
        let raw = sm.slice(t.span).unwrap_or("");
        let display = match t.kind {
            TokenKind::Eof => "<EOF>".to_string(),
            _ => {
                let escaped: String = raw
                    .chars()
                    .map(|c| match c {
                        '\n' => '⏎',
                        '\t' => '→',
                        c => c,
                    })
                    .collect();
                let trimmed: String = escaped.chars().take(40).collect();
                let suffix = if escaped.chars().count() > 40 {
                    "…"
                } else {
                    ""
                };
                format!("`{trimmed}{suffix}`")
            }
        };
        println!(
            "{i:>4}  {line:>3}:{col:<3}  {kind:<14} {display}",
            kind = format!("{:?}", t.kind)
        );
    }

    // ── Diagnostics (should be empty) ──────────────────────────────────
    if !diags.is_empty() {
        println!("\n--- diagnostics ---");
        for d in diags.iter() {
            println!("  {}", render_simple(d, &sm));
        }
    }

    assert!(
        !diags.has_errors(),
        "fixture must lex with zero error-severity diagnostics"
    );
}
