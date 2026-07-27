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
    LayoutIntroducer,
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

    pub(super) const fn roles(self) -> &'static [KeywordRole] {
        self.roles
    }
}

use KeywordRole::{
    Declaration, Expression, LayoutIntroducer, Modifier, ObjectAlias, Operator, SpecialReference,
    Statement,
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
        roles: &[Expression, LayoutIntroducer],
    },
    Keyword {
        spelling: "else",
        roles: &[Modifier],
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
        spelling: "if",
        roles: &[Expression],
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
        roles: &[Expression, LayoutIntroducer],
    },
    Keyword {
        spelling: "match",
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
        spelling: "then",
        roles: &[Modifier],
    },
    Keyword {
        spelling: "try",
        roles: &[Expression],
    },
    Keyword {
        spelling: "try_match",
        roles: &[Expression],
    },
    Keyword {
        spelling: "unique",
        roles: &[Declaration],
    },
    Keyword {
        spelling: "using",
        roles: &[Expression],
    },
    Keyword {
        spelling: "when",
        roles: &[Modifier, LayoutIntroducer],
    },
    Keyword {
        spelling: "where",
        roles: &[Expression, LayoutIntroducer],
    },
    Keyword {
        spelling: "with",
        roles: &[Modifier, LayoutIntroducer],
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

pub(super) fn g0_layout_introducer(name: &str) -> bool {
    g0_keyword(name).is_some_and(|keyword| keyword.roles().contains(&LayoutIntroducer))
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
            Some(&[Expression, LayoutIntroducer][..])
        );
        assert_eq!(
            g0_keyword("with").map(Keyword::roles),
            Some(&[Modifier, LayoutIntroducer][..])
        );
        assert_eq!(
            g0_keyword("if").map(Keyword::roles),
            Some(&[Expression][..])
        );
        assert_eq!(
            g0_keyword("match").map(Keyword::roles),
            Some(&[Expression][..])
        );
        assert_eq!(
            g0_keyword("when").map(Keyword::roles),
            Some(&[Modifier, LayoutIntroducer][..])
        );
        assert_eq!(
            g0_keyword("try").map(Keyword::roles),
            Some(&[Expression][..])
        );
        assert_eq!(
            g0_keyword("try_match").map(Keyword::roles),
            Some(&[Expression][..])
        );
        assert_eq!(
            g0_keyword("self").map(Keyword::roles),
            Some(&[SpecialReference, ObjectAlias][..])
        );
        assert!(
            ["do", "let", "when", "where", "with"]
                .into_iter()
                .all(g0_layout_introducer)
        );
        assert!(
            !["if", "match", "object"]
                .into_iter()
                .any(g0_layout_introducer)
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
