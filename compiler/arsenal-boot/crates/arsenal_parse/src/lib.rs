//! GW recursive-descent parser with Pratt-style expression precedence.
//!
//! See `docs/architecture.md` Part B.3 (parser) and Part B.4
//! (single-pass with backpatching).
//!
//! Public entry point: [`parse`].
//!
//! ```ignore
//! use arsenal_parse::parse;
//! use arsenal_ast::FileArena;
//! use arsenal_lex::SourceMap;
//! use bumpalo::Bump;
//!
//! let mut sm = SourceMap::new();
//! let file = sm.add_file("hello.gw", "fn main() {}");
//! let src = sm.get(file).unwrap().contents.as_bytes();
//! let bump = Bump::new();
//! let arena = FileArena::new(&bump, file);
//! let (root, diags) = parse(file, src, &arena);
//! assert!(!diags.has_errors());
//! ```

mod grammar;
mod parser;
mod recovery;

pub use parser::ec;
pub use parser::Parser;

use arsenal_ast::{FileArena, SyntaxNode};
use arsenal_lex::{lex, DiagBag, FileId};

/// Lex `src` and parse the resulting token stream into a CST module
/// rooted in `arena`. Returns the root node and a diagnostic bag
/// containing both lexer and parser diagnostics.
pub fn parse<'bump>(
    file: FileId,
    src: &[u8],
    arena: &FileArena<'bump>,
) -> (&'bump SyntaxNode<'bump>, DiagBag) {
    let (tokens, diags) = lex(file, src);
    let mut p = Parser::new(src, &tokens, arena, diags);
    grammar::parse_module(&mut p);
    let root = p
        .builder
        .finish_root(p.cur_byte_start())
        // `parse_module` is responsible for opening the Module frame, so
        // this is unreachable in practice. We synthesise an empty Module
        // for safety rather than panicking.
        .unwrap_or_else(|| {
            // Rebuild a minimal Module from scratch.
            let mut b = arsenal_ast::CstBuilder::new(arena);
            b.start_node(arsenal_ast::SyntaxKind::Module, 0);
            b.finish_root(0).expect("just opened")
        });
    (root, p.diags)
}
