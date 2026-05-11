//! Concrete syntax tree (CST) data structures and the builder used by the
//! parser to construct a CST while consuming a token stream.
//!
//! See `docs/architecture.md` Part B.3 (CST + AST split) and Part C.2
//! (typed AST as a view over the CST).
//!
//! ## Properties
//!
//! - **Lossless**: every byte of source maps to exactly one CST leaf
//!   (token), including whitespace, comments, and the synthetic `Eof`.
//!   Concatenating `sm.slice(leaf.span)` over the whole tree in source
//!   order reproduces the file.
//! - **Immutable**: `SyntaxNode` is `Copy`; once allocated in the bump
//!   arena it is never modified.
//! - **No parent pointer**: navigation is top-down; the LSP / formatter
//!   later phase adds a "red layer" that synthesises parent links on
//!   demand without touching this representation.

use crate::arena::FileArena;
use crate::syntax_kind::SyntaxKind;
use gw_lex::{FileId, Span};

/// A composite CST node.
///
/// Spans cover all children including trivia; the union of every leaf's
/// span equals the node's span.
#[derive(Copy, Clone, Debug)]
pub struct SyntaxNode<'a> {
    /// What kind of construct this node represents.
    pub kind: SyntaxKind,
    /// Source range covered by this node and all its children.
    pub span: Span,
    /// Children in source order: a mix of leaf tokens and child nodes.
    pub children: &'a [SyntaxElement<'a>],
}

impl<'a> SyntaxNode<'a> {
    /// Iterate over child node references, skipping leaf tokens.
    pub fn child_nodes(&self) -> impl Iterator<Item = &'a SyntaxNode<'a>> + '_ {
        self.children.iter().filter_map(|c| match c {
            SyntaxElement::Node(n) => Some(*n),
            SyntaxElement::Token { .. } => None,
        })
    }

    /// Iterate over leaf tokens, skipping child nodes.
    pub fn child_tokens(&self) -> impl Iterator<Item = (SyntaxKind, Span)> + '_ {
        self.children.iter().filter_map(|c| match c {
            SyntaxElement::Token { kind, span } => Some((*kind, *span)),
            SyntaxElement::Node(_) => None,
        })
    }

    /// First child node whose [`SyntaxKind`] equals `kind`.
    pub fn child_node(&self, kind: SyntaxKind) -> Option<&'a SyntaxNode<'a>> {
        self.child_nodes().find(|n| n.kind == kind)
    }

    /// First child node satisfying the predicate.
    pub fn child_node_where(
        &self,
        pred: impl FnMut(&&'a SyntaxNode<'a>) -> bool,
    ) -> Option<&'a SyntaxNode<'a>> {
        self.child_nodes().find(pred)
    }

    /// First child token of the given kind, returned as `(kind, span)`.
    pub fn child_token(&self, kind: SyntaxKind) -> Option<Span> {
        self.children.iter().find_map(|c| match c {
            SyntaxElement::Token { kind: k, span } if *k == kind => Some(*span),
            _ => None,
        })
    }
}

/// One child of a [`SyntaxNode`]: either a leaf token (with its kind and
/// source span) or a child node reference.
///
/// Token text is recovered by slicing the source via the
/// [`gw_lex::SourceMap`]; this keeps `SyntaxElement` cheap to copy
/// and prevents the CST from holding `&str` references to source bytes.
#[derive(Copy, Clone, Debug)]
pub enum SyntaxElement<'a> {
    /// Leaf token. `kind` is a token-side `SyntaxKind` (`Ident`, `Semi`,
    /// `KwFn`, …); `span` holds its source location.
    Token {
        /// What kind of token.
        kind: SyntaxKind,
        /// Source range of the token's lexeme.
        span: Span,
    },
    /// Child subtree.
    Node(&'a SyntaxNode<'a>),
}

impl<'a> SyntaxElement<'a> {
    /// Source range covered by this element.
    pub fn span(&self) -> Span {
        match self {
            Self::Token { span, .. } => *span,
            Self::Node(n) => n.span,
        }
    }

    /// `SyntaxKind` of this element (token kind or node kind).
    pub fn kind(&self) -> SyntaxKind {
        match self {
            Self::Token { kind, .. } => *kind,
            Self::Node(n) => n.kind,
        }
    }
}

/// Helper used by the parser to construct CST nodes incrementally.
///
/// Tracks one open node at a time on a stack; child elements (tokens or
/// previously-finished nodes) are buffered into a temporary [`Vec`] until
/// [`CstBuilder::finish_node`] copies them into the bump arena and emits
/// a fresh [`SyntaxNode`].
///
/// The builder is scoped to one source file. The caller owns a
/// [`FileArena`] and feeds it in.
pub struct CstBuilder<'arena, 'bump> {
    arena: &'arena FileArena<'bump>,
    /// Stack of in-progress nodes. Each frame holds the kind being built
    /// plus the children accumulated so far.
    frames: Vec<Frame<'bump>>,
    /// Whether [`CstBuilder::finish_root`] has been called.
    finished: bool,
}

struct Frame<'bump> {
    kind: SyntaxKind,
    /// Span start byte. Span end is determined when the frame finishes.
    start: u32,
    children: Vec<SyntaxElement<'bump>>,
}

impl<'arena, 'bump> CstBuilder<'arena, 'bump> {
    /// Construct a new builder bound to `arena`. Call
    /// [`Self::start_node`] to open the root.
    pub fn new(arena: &'arena FileArena<'bump>) -> Self {
        Self {
            arena,
            frames: Vec::new(),
            finished: false,
        }
    }

    /// Open a new node at byte position `start_pos`. All subsequently
    /// pushed elements until the matching [`Self::finish_node`] become
    /// children of this node.
    pub fn start_node(&mut self, kind: SyntaxKind, start_pos: u32) {
        debug_assert!(!self.finished, "builder already finished");
        self.frames.push(Frame {
            kind,
            start: start_pos,
            children: Vec::new(),
        });
    }

    /// Append a leaf token to the currently-open node.
    pub fn push_token(&mut self, kind: SyntaxKind, span: Span) {
        let frame = self
            .frames
            .last_mut()
            .expect("push_token without an open node");
        frame.children.push(SyntaxElement::Token { kind, span });
    }

    /// Close the currently-open node, allocate it in the arena, and (if
    /// there is a parent frame) attach it to the parent. The closed
    /// node's span ends at byte position `end_pos`.
    ///
    /// Returns the freshly allocated node reference for the caller's use.
    /// When closing the root frame, no parent attachment happens; the
    /// caller should retrieve the root via [`Self::finish_root`].
    pub fn finish_node(&mut self, end_pos: u32) -> &'bump SyntaxNode<'bump> {
        let frame = self.frames.pop().expect("finish_node without open node");
        let span = Span::new(self.file_id(), frame.start, end_pos);
        let children = self.arena.alloc_children(&frame.children);
        let node = self.arena.alloc_node(SyntaxNode {
            kind: frame.kind,
            span,
            children,
        });
        if let Some(parent) = self.frames.last_mut() {
            parent.children.push(SyntaxElement::Node(node));
        }
        node
    }

    /// Close the root node. Asserts the stack is empty after the close.
    ///
    /// Returns `None` if no root was started, or `Some(root)` exactly
    /// once. Subsequent calls return `None`.
    pub fn finish_root(&mut self, end_pos: u32) -> Option<&'bump SyntaxNode<'bump>> {
        if self.finished {
            return None;
        }
        if self.frames.is_empty() {
            return None;
        }
        let root = self.finish_node(end_pos);
        debug_assert!(self.frames.is_empty(), "frames remain after finish_root");
        self.finished = true;
        Some(root)
    }

    fn file_id(&self) -> FileId {
        self.arena.file
    }

    /// Whether at least one frame is currently open.
    pub fn has_open_node(&self) -> bool {
        !self.frames.is_empty()
    }

    /// Record a position in the currently-open frame's child list.
    ///
    /// Used by the Pratt expression parser: parse the left operand,
    /// take a checkpoint, decide whether an infix operator follows, and
    /// (if so) call [`Self::start_node_at`] to wrap the left operand
    /// retroactively in a [`SyntaxKind::BinaryExpr`] node.
    pub fn checkpoint(&self) -> Checkpoint {
        let frame = self.frames.last().expect("checkpoint without an open node");
        Checkpoint {
            frame_idx: self.frames.len() - 1,
            child_idx: frame.children.len(),
        }
    }

    /// Open a new node at a previously-recorded [`Checkpoint`]. Children
    /// added to the current frame after the checkpoint become the
    /// children of the new node.
    ///
    /// Panics in debug builds if the frame stack has grown since the
    /// checkpoint was taken (the parser is responsible for not
    /// `start_node`ing in between).
    pub fn start_node_at(&mut self, cp: Checkpoint, kind: SyntaxKind, start_pos: u32) {
        debug_assert_eq!(
            self.frames.len() - 1,
            cp.frame_idx,
            "start_node_at across a different frame than the checkpoint"
        );
        let frame = self
            .frames
            .last_mut()
            .expect("start_node_at without an open node");
        debug_assert!(
            cp.child_idx <= frame.children.len(),
            "checkpoint child_idx out of range"
        );
        // Move the post-checkpoint children into the new frame.
        let moved: Vec<_> = frame.children.drain(cp.child_idx..).collect();
        self.frames.push(Frame {
            kind,
            start: start_pos,
            children: moved,
        });
    }
}

/// Marker recording a position in the open frame's child list. Created
/// by [`CstBuilder::checkpoint`] and consumed by
/// [`CstBuilder::start_node_at`].
#[derive(Copy, Clone, Debug)]
pub struct Checkpoint {
    frame_idx: usize,
    child_idx: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bumpalo::Bump;

    fn span(start: u32, end: u32) -> Span {
        Span::new(FileId::NONE, start, end)
    }

    #[test]
    fn build_a_trivial_tree() {
        let bump = Bump::new();
        let arena = FileArena::new(&bump, FileId::NONE);
        let mut b = CstBuilder::new(&arena);

        // Module { FnDecl { KwFn, Ident, Block { LBrace, RBrace } } }
        b.start_node(SyntaxKind::Module, 0);
        b.start_node(SyntaxKind::FnDecl, 0);
        b.push_token(SyntaxKind::KwFn, span(0, 2));
        b.push_token(SyntaxKind::Whitespace, span(2, 3));
        b.push_token(SyntaxKind::Ident, span(3, 7));
        b.push_token(SyntaxKind::Whitespace, span(7, 8));
        b.start_node(SyntaxKind::Block, 8);
        b.push_token(SyntaxKind::LBrace, span(8, 9));
        b.push_token(SyntaxKind::RBrace, span(9, 10));
        b.finish_node(10);
        b.finish_node(10);
        let root = b.finish_root(10).expect("root present");

        assert_eq!(root.kind, SyntaxKind::Module);
        assert_eq!(root.span, span(0, 10));

        let fn_decl = root.child_node(SyntaxKind::FnDecl).expect("fn decl");
        assert_eq!(fn_decl.kind, SyntaxKind::FnDecl);

        let block = fn_decl.child_node(SyntaxKind::Block).expect("block");
        assert!(block.child_token(SyntaxKind::LBrace).is_some());
        assert!(block.child_token(SyntaxKind::RBrace).is_some());

        // child_tokens iterates leaf tokens only.
        let toks: Vec<_> = fn_decl.child_tokens().map(|(k, _)| k).collect();
        assert_eq!(
            toks,
            vec![
                SyntaxKind::KwFn,
                SyntaxKind::Whitespace,
                SyntaxKind::Ident,
                SyntaxKind::Whitespace,
            ]
        );
    }

    #[test]
    fn finish_root_returns_none_when_empty() {
        let bump = Bump::new();
        let arena = FileArena::new(&bump, FileId::NONE);
        let mut b = CstBuilder::new(&arena);
        assert!(b.finish_root(0).is_none());
    }

    #[test]
    fn checkpoint_and_start_node_at_wrap_lhs() {
        let bump = Bump::new();
        let arena = FileArena::new(&bump, FileId::NONE);
        let mut b = CstBuilder::new(&arena);

        // Module { a + b }  parsed left-associatively:
        //   start Module
        //   parse a as PathExpr child of Module
        //   checkpoint (now a is at child_idx 0)
        //   see `+`, decide to wrap
        //   start_node_at(cp, BinaryExpr) -> moves PathExpr(a) into BinaryExpr
        //   bump `+`
        //   parse b as PathExpr child of BinaryExpr
        //   finish BinaryExpr -> Module's only child is BinaryExpr
        b.start_node(SyntaxKind::Module, 0);
        // Take checkpoint BEFORE parsing the LHS so that wrapping moves
        // the LHS subtree into the new BinaryExpr.
        let cp = b.checkpoint();
        // PathExpr(a)
        b.start_node(SyntaxKind::PathExpr, 0);
        b.push_token(SyntaxKind::Ident, span(0, 1));
        b.finish_node(1);
        // wrap into BinaryExpr starting at byte 0
        b.start_node_at(cp, SyntaxKind::BinaryExpr, 0);
        b.push_token(SyntaxKind::Plus, span(2, 3));
        b.start_node(SyntaxKind::PathExpr, 4);
        b.push_token(SyntaxKind::Ident, span(4, 5));
        b.finish_node(5);
        b.finish_node(5);
        let root = b.finish_root(5).expect("root");
        assert_eq!(root.kind, SyntaxKind::Module);
        assert_eq!(root.children.len(), 1);
        let bin = root.child_node(SyntaxKind::BinaryExpr).expect("binary");
        let kinds: Vec<_> = bin.children.iter().map(|c| c.kind()).collect();
        assert_eq!(
            kinds,
            vec![SyntaxKind::PathExpr, SyntaxKind::Plus, SyntaxKind::PathExpr],
        );
    }
}
