//! The filter dropdown next to the search field (Everything’s “Search: Everything / Audio / …”).

use serde::{Deserialize, Serialize};

/// A filter: a name and a query that results must also match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    pub name: String,
    pub query: String,
}

/// The built-in filters (their names are translated by the frontends).
#[must_use]
pub fn builtin() -> Vec<Filter> {
    [
        ("Everything", ""),
        ("Audio", "audio:"),
        ("Compressed", "zip:"),
        ("Document", "doc:"),
        ("Executable", "exe:"),
        ("Folder", "folder:"),
        ("Picture", "pic:"),
        ("Video", "video:"),
    ]
    .into_iter()
    .map(|(name, query)| Filter {
        name: name.into(),
        query: query.into(),
    })
    .collect()
}

/// The query actually sent: the filter’s terms AND the user’s text. Space is the lowest
/// precedence operator, so plain concatenation keeps `a | b` meaning `filter AND (a | b)`.
#[must_use]
pub fn combine(filter: &str, text: &str) -> String {
    match (filter.trim(), text.trim()) {
        ("", t) => t.to_owned(),
        (f, "") => f.to_owned(),
        (f, t) => format!("{f} {t}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combining() {
        assert_eq!(combine("", " foo "), "foo");
        assert_eq!(combine("audio:", ""), "audio:");
        assert_eq!(combine("audio:", "a | b"), "audio: a | b");
        assert_eq!(builtin().len(), 8);
        assert_eq!(builtin()[0].query, "");
    }
}
