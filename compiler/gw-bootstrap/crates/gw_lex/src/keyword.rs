//! Keyword perfect-hash table.
//!
//! See `docs/architecture.md` Part B.2 ("Reserved words are interned at
//! startup into a `phf` perfect-hash map").

use crate::token::TokenKind;
use phf::phf_map;

/// Maps the source text of a keyword (or reserved theme alias) to its
/// [`TokenKind`]. Constant-time lookup via a `phf` perfect hash.
///
/// Updates here must keep [`TokenKind::is_keyword`] and
/// [`TokenKind::as_str`] in sync.
pub static KEYWORDS: phf::Map<&'static str, TokenKind> = phf_map! {
    "fn"        => TokenKind::KwFn,
    "let"       => TokenKind::KwLet,
    "var"       => TokenKind::KwVar,
    "const"     => TokenKind::KwConst,
    "class"     => TokenKind::KwClass,
    "liberty"   => TokenKind::KwLiberty,
    "cipher"    => TokenKind::KwCipher,
    "if"        => TokenKind::KwIf,
    "else"      => TokenKind::KwElse,
    "match"     => TokenKind::KwMatch,
    "for"       => TokenKind::KwFor,
    "while"     => TokenKind::KwWhile,
    "loop"      => TokenKind::KwLoop,
    "break"     => TokenKind::KwBreak,
    "continue"  => TokenKind::KwContinue,
    "return"    => TokenKind::KwReturn,
    "defer"     => TokenKind::KwDefer,
    "errdefer"  => TokenKind::KwErrdefer,
    "try"       => TokenKind::KwTry,
    "catch"     => TokenKind::KwCatch,
    "foxdie"    => TokenKind::KwFoxdie,
    "naked"     => TokenKind::KwNaked,
    "pub"       => TokenKind::KwPub,
    "mod"       => TokenKind::KwMod,
    "use"       => TokenKind::KwUse,
    "as"        => TokenKind::KwAs,
    "in"        => TokenKind::KwIn,
    "where"     => TokenKind::KwWhere,
    "comptime"  => TokenKind::KwComptime,
    "inline"    => TokenKind::KwInline,
    "extern"    => TokenKind::KwExtern,
    "rex"       => TokenKind::KwRex,
    "lock"      => TokenKind::KwLock,
    "fox"       => TokenKind::KwFox,
    "await"     => TokenKind::KwAwait,
    "yield"     => TokenKind::KwYield,
    "true"      => TokenKind::KwTrue,
    "false"     => TokenKind::KwFalse,
    "nil"       => TokenKind::KwNil,
    "enum"      => TokenKind::KwEnum,
    "union"     => TokenKind::KwUnion,
    "unsafe"    => TokenKind::KwUnsafe,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_basic() {
        assert_eq!(KEYWORDS.get("fn").copied(), Some(TokenKind::KwFn));
        assert_eq!(KEYWORDS.get("foxdie").copied(), Some(TokenKind::KwFoxdie));
        assert_eq!(KEYWORDS.get("notakeyword").copied(), None);
        assert_eq!(KEYWORDS.get("").copied(), None);
    }

    #[test]
    fn theme_aliases_reserved() {
        assert_eq!(KEYWORDS.get("enum").copied(), Some(TokenKind::KwEnum));
        assert_eq!(KEYWORDS.get("union").copied(), Some(TokenKind::KwUnion));
        assert_eq!(KEYWORDS.get("unsafe").copied(), Some(TokenKind::KwUnsafe));
    }

    #[test]
    fn as_str_round_trip_for_every_keyword() {
        // Every entry in the phf table must round-trip through
        // TokenKind::as_str.
        for (text, kind) in KEYWORDS.entries() {
            assert_eq!(kind.as_str(), Some(*text), "keyword {text} mismatch");
        }
    }
}
