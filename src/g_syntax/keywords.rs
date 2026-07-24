//! Language-version-owned source keywords.
//!
//! The bootstrap currently implements one grammar, `g0`. Keep its reserved
//! words here rather than letting individual parser productions grow their own
//! contextual lists.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeywordRole {
    Declaration,
    Expression,
    Statement,
    Operator,
    Modifier,
    ObjectAlias,
    SpecialReference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Keyword {
    spelling: &'static str,
    roles: &'static [KeywordRole],
}

impl Keyword {
    pub(super) const fn spelling(self) -> &'static str {
        self.spelling
    }

    #[cfg(test)]
    pub(super) const fn roles(self) -> &'static [KeywordRole] {
        self.roles
    }
}

use KeywordRole::{
    Declaration, Expression, Modifier, ObjectAlias, Operator, SpecialReference, Statement,
};

pub(super) const G0_KEYWORDS: &[Keyword] = &[
    Keyword {
        spelling: "abstract",
        roles: &[Declaration, Expression, Statement, Modifier],
    },
    Keyword {
        spelling: "and",
        roles: &[Operator],
    },
    Keyword {
        spelling: "as",
        roles: &[Modifier],
    },
    Keyword {
        spelling: "at",
        roles: &[Modifier],
    },
    Keyword {
        spelling: "binary",
        roles: &[Modifier],
    },
    Keyword {
        spelling: "do",
        roles: &[Expression],
    },
    Keyword {
        spelling: "extend",
        roles: &[Declaration],
    },
    Keyword {
        spelling: "extends",
        roles: &[Modifier],
    },
    Keyword {
        spelling: "import",
        roles: &[Declaration],
    },
    Keyword {
        spelling: "in",
        roles: &[Modifier],
    },
    Keyword {
        spelling: "language",
        roles: &[Declaration],
    },
    Keyword {
        spelling: "let",
        roles: &[Expression],
    },
    Keyword {
        spelling: "module",
        roles: &[SpecialReference],
    },
    Keyword {
        spelling: "object",
        roles: &[Declaration, Expression],
    },
    Keyword {
        spelling: "or",
        roles: &[Operator],
    },
    Keyword {
        spelling: "self",
        roles: &[SpecialReference, ObjectAlias],
    },
    Keyword {
        spelling: "unique",
        roles: &[Declaration],
    },
    Keyword {
        spelling: "where",
        roles: &[Expression],
    },
    Keyword {
        spelling: "with",
        roles: &[Modifier],
    },
];

pub(super) fn g0_keyword(name: &str) -> Option<Keyword> {
    G0_KEYWORDS
        .binary_search_by_key(&name, |keyword| keyword.spelling)
        .ok()
        .map(|index| G0_KEYWORDS[index])
}

pub(super) fn canonical_keyword(name: &str) -> Option<Keyword> {
    let canonical = name
        .strip_prefix('_')
        .filter(|name| !name.is_empty())
        .unwrap_or(name);
    g0_keyword(canonical)
}

pub(super) fn reserved_keyword_message(keyword: Keyword) -> String {
    let spelling = keyword.spelling();
    format!(
        "`{spelling}` is a reserved keyword in language `g0`; use `'{spelling}` for atom data or `.['{spelling}]` for a path component"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g0_keyword_table_is_sorted_and_unique() {
        assert!(
            G0_KEYWORDS
                .windows(2)
                .all(|pair| pair[0].spelling() < pair[1].spelling())
        );
    }

    #[test]
    fn keyword_roles_record_each_active_syntax_site() {
        assert_eq!(
            g0_keyword("abstract").map(Keyword::roles),
            Some(&[Declaration, Expression, Statement, Modifier][..])
        );
        assert_eq!(
            g0_keyword("object").map(Keyword::roles),
            Some(&[Declaration, Expression][..])
        );
        assert_eq!(
            g0_keyword("where").map(Keyword::roles),
            Some(&[Expression][..])
        );
        assert_eq!(
            g0_keyword("with").map(Keyword::roles),
            Some(&[Modifier][..])
        );
        assert_eq!(
            g0_keyword("self").map(Keyword::roles),
            Some(&[SpecialReference, ObjectAlias][..])
        );
    }

    #[test]
    fn suppressed_local_spellings_retain_keyword_identity() {
        assert_eq!(
            canonical_keyword("_where").map(Keyword::spelling),
            Some("where")
        );
        assert_eq!(canonical_keyword("_"), None);
    }
}
