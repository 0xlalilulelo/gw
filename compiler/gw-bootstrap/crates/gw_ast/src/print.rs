//! Pretty-printer for CST nodes — produces an indented, line-per-element
//! dump used by `gw build` to satisfy the Phase 0 exit criterion
//! ("typed AST dump").
//!
//! The format is deliberately stable and grep-friendly:
//!
//! ```text
//! Module @1:1..238:1
//!   FnDecl @7:1..9:2
//!     DocComment `/// Sum-type smoke test`
//!     KwFn `fn`
//!     Ident `dot`
//!     ParamList @7:8..7:31
//!       Param @7:9..7:18
//!         Ident `a`
//!         RefType @7:12..7:18
//!           PathType @7:13..7:17
//!             Ident `Vec3`
//! ```
//!
//! By default whitespace and ordinary line/block comments are elided to
//! keep output compact; doc comments are kept because they are
//! semantically attached to the following item. Tweak via [`DumpOpts`].

use crate::cst::{SyntaxElement, SyntaxNode};
use crate::syntax_kind::SyntaxKind;
use gw_lex::SourceMap;
use std::fmt::Write;

/// Options controlling [`dump_with`].
#[derive(Copy, Clone, Debug)]
pub struct DumpOpts {
    /// Show `Whitespace` leaves.
    pub include_whitespace: bool,
    /// Show `LineComment` and `BlockComment` leaves (not `DocComment`).
    pub include_comments: bool,
    /// Show `DocComment` leaves.
    pub include_doc_comments: bool,
    /// Show `name:line:col..line:col` after each node.
    pub include_spans: bool,
}

impl Default for DumpOpts {
    fn default() -> Self {
        Self {
            include_whitespace: false,
            include_comments: false,
            include_doc_comments: true,
            include_spans: true,
        }
    }
}

/// Dump `node` with default options.
pub fn dump(node: &SyntaxNode<'_>, sm: &SourceMap) -> String {
    dump_with(node, sm, DumpOpts::default())
}

/// Dump `node` with the given options.
pub fn dump_with(node: &SyntaxNode<'_>, sm: &SourceMap, opts: DumpOpts) -> String {
    let mut out = String::new();
    write_node(&mut out, node, sm, &opts, 0);
    out
}

fn write_node(
    out: &mut String,
    node: &SyntaxNode<'_>,
    sm: &SourceMap,
    opts: &DumpOpts,
    depth: usize,
) {
    write_indent(out, depth);
    let _ = write!(out, "{:?}", node.kind);
    if opts.include_spans {
        write_span(out, node.span.start, node.span.end, sm, node.span.file);
    }
    out.push('\n');
    for c in node.children {
        match c {
            SyntaxElement::Token { kind, span } => {
                if !should_emit(*kind, opts) {
                    continue;
                }
                write_indent(out, depth + 1);
                let _ = write!(out, "{:?}", kind);
                let raw = sm.slice(*span).unwrap_or("");
                let display = render_token_text(*kind, raw);
                if !display.is_empty() {
                    let _ = write!(out, " `{display}`");
                }
                if opts.include_spans {
                    write_span(out, span.start, span.end, sm, span.file);
                }
                out.push('\n');
            }
            SyntaxElement::Node(child) => {
                write_node(out, child, sm, opts, depth + 1);
            }
        }
    }
}

fn should_emit(kind: SyntaxKind, opts: &DumpOpts) -> bool {
    match kind {
        SyntaxKind::Whitespace => opts.include_whitespace,
        SyntaxKind::LineComment | SyntaxKind::BlockComment => opts.include_comments,
        SyntaxKind::DocComment => opts.include_doc_comments,
        _ => true,
    }
}

fn render_token_text(kind: SyntaxKind, raw: &str) -> String {
    match kind {
        SyntaxKind::Eof => String::new(),
        SyntaxKind::Whitespace => "<ws>".to_string(),
        SyntaxKind::LineComment | SyntaxKind::BlockComment | SyntaxKind::DocComment => {
            // Truncate and inline-newline-escape so dump stays one line per leaf.
            let s: String = raw
                .chars()
                .take(60)
                .map(|c| if c == '\n' { '⏎' } else { c })
                .collect();
            if raw.chars().count() > 60 {
                format!("{s}…")
            } else {
                s
            }
        }
        _ => raw
            .chars()
            .map(|c| match c {
                '\n' => '⏎',
                '\t' => '→',
                c => c,
            })
            .collect(),
    }
}

fn write_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_span(out: &mut String, start: u32, end: u32, sm: &SourceMap, file: gw_lex::FileId) {
    let Some(f) = sm.get(file) else {
        let _ = write!(out, " @{start}..{end}");
        return;
    };
    let (sl, sc) = f.line_col(start);
    let (el, ec) = f.line_col(end);
    let _ = write!(out, " @{sl}:{sc}..{el}:{ec}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::FileArena;
    use crate::cst::CstBuilder;
    use bumpalo::Bump;
    use gw_lex::{SourceMap, Span};

    #[test]
    fn dump_simple_tree() {
        let mut sm = SourceMap::new();
        let file = sm.add_file("t.gw", "fn f() {}");
        let bump = Bump::new();
        let arena = FileArena::new(&bump, file);
        let mut b = CstBuilder::new(&arena);
        b.start_node(SyntaxKind::Module, 0);
        b.start_node(SyntaxKind::FnDecl, 0);
        b.push_token(SyntaxKind::KwFn, Span::new(file, 0, 2));
        b.push_token(SyntaxKind::Whitespace, Span::new(file, 2, 3));
        b.push_token(SyntaxKind::Ident, Span::new(file, 3, 4));
        b.push_token(SyntaxKind::LParen, Span::new(file, 4, 5));
        b.push_token(SyntaxKind::RParen, Span::new(file, 5, 6));
        b.push_token(SyntaxKind::Whitespace, Span::new(file, 6, 7));
        b.start_node(SyntaxKind::Block, 7);
        b.push_token(SyntaxKind::LBrace, Span::new(file, 7, 8));
        b.push_token(SyntaxKind::RBrace, Span::new(file, 8, 9));
        b.finish_node(9);
        b.finish_node(9);
        let root = b.finish_root(9).expect("root");
        let s = dump(root, &sm);
        // Whitespace elided, structure visible.
        assert!(s.contains("Module"));
        assert!(s.contains("FnDecl"));
        assert!(s.contains("KwFn `fn`"));
        assert!(s.contains("Ident `f`"));
        assert!(s.contains("Block"));
        assert!(!s.contains("Whitespace"));
    }

    #[test]
    fn whitespace_visible_with_opts() {
        let mut sm = SourceMap::new();
        let file = sm.add_file("t.gw", " ");
        let bump = Bump::new();
        let arena = FileArena::new(&bump, file);
        let mut b = CstBuilder::new(&arena);
        b.start_node(SyntaxKind::Module, 0);
        b.push_token(SyntaxKind::Whitespace, Span::new(file, 0, 1));
        let root = b.finish_root(1).expect("root");
        let s = dump_with(
            root,
            &sm,
            DumpOpts {
                include_whitespace: true,
                ..DumpOpts::default()
            },
        );
        assert!(s.contains("Whitespace"));
    }
}
