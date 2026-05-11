//! Per-file arena over [`bumpalo::Bump`] used to allocate CST nodes and
//! their child slices.
//!
//! See `docs/architecture.md` Part B.3 ("AST is a typed view over the CST")
//! and the Phase-0 deliverables note: "Arena allocation via bumpalo for
//! AST/CST nodes — one arena per file."
//!
//! The bump itself is owned by the caller (typically the parser entry
//! point or the driver). Constructed `SyntaxNode`s and child slices borrow
//! from the bump; dropping the bump releases the entire CST in O(1).

use crate::cst::{SyntaxElement, SyntaxNode};
use bumpalo::Bump;
use gw_lex::FileId;

/// Allocates CST nodes and child slices for one source file.
///
/// `'bump` is the lifetime of the underlying [`Bump`]; every CST value
/// produced by this arena is bounded by `'bump`.
#[derive(Copy, Clone)]
pub struct FileArena<'bump> {
    bump: &'bump Bump,
    /// Identifier of the file whose CST lives in this arena. Stored so
    /// later passes can recover the source file without an out-of-band
    /// channel.
    pub file: FileId,
}

impl<'bump> FileArena<'bump> {
    /// Construct a new arena view over `bump`, tagged with `file`.
    pub fn new(bump: &'bump Bump, file: FileId) -> Self {
        Self { bump, file }
    }

    /// Allocate a single [`SyntaxNode`] in the bump and return a borrowed
    /// reference to it.
    pub fn alloc_node(&self, node: SyntaxNode<'bump>) -> &'bump SyntaxNode<'bump> {
        self.bump.alloc(node)
    }

    /// Copy `els` into the bump and return the resulting slice.
    pub fn alloc_children(&self, els: &[SyntaxElement<'bump>]) -> &'bump [SyntaxElement<'bump>] {
        self.bump.alloc_slice_copy(els)
    }

    /// Borrow the underlying bump. Useful for callers that want to
    /// allocate non-CST data in the same arena.
    pub fn bump(&self) -> &'bump Bump {
        self.bump
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cst::SyntaxElement;
    use crate::syntax_kind::SyntaxKind;
    use gw_lex::Span;

    #[test]
    fn alloc_node_and_children() {
        let bump = Bump::new();
        let file = FileId::NONE;
        let arena = FileArena::new(&bump, file);

        let kw = SyntaxElement::Token {
            kind: SyntaxKind::KwFn,
            span: Span::new(file, 0, 2),
        };
        let children = arena.alloc_children(&[kw]);
        let node = arena.alloc_node(SyntaxNode {
            kind: SyntaxKind::FnDecl,
            span: Span::new(file, 0, 2),
            children,
        });
        assert_eq!(node.kind, SyntaxKind::FnDecl);
        assert_eq!(node.children.len(), 1);
    }
}
