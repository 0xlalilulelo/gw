//! Synchronisation sets and bracket-counted skip used for panic-mode
//! error recovery.
//!
//! See `docs/architecture.md` Part B.3: "panic-mode at statement
//! boundaries; bracket-counted skip".

use arsenal_lex::TokenKind;

/// Tokens at which the parser may resume parsing items after an error.
///
/// `Eof` is included so recovery always terminates.
pub fn is_item_start(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::KwFn
            | TokenKind::KwClass
            | TokenKind::KwLiberty
            | TokenKind::KwCipher
            | TokenKind::KwConst
            | TokenKind::KwMod
            | TokenKind::KwUse
            | TokenKind::KwExtern
            | TokenKind::KwPub
            | TokenKind::KwInline
            | TokenKind::KwComptime
            | TokenKind::Hash
            | TokenKind::Eof
    )
}

/// Tokens at which the parser may resume parsing statements after an
/// error. Items are super-sync points (a stmt cannot cross an item
/// boundary), so item-start kinds are also stmt boundaries.
pub fn is_stmt_boundary(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Semi | TokenKind::RBrace) || is_item_start(kind)
}
