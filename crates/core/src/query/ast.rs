//! Query syntax tree and its canonical printer.

use std::fmt::{self, Write as _};

/// Restricts a term to files or folders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KindOnly {
    Files,
    Folders,
    Any,
}

/// Per-term search modifiers. `None` means “use the global search option”.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub case: Option<bool>,
    pub whole_word: Option<bool>,
    pub path: Option<bool>,
    pub regex: Option<bool>,
    /// `Some(true)` = match diacritics exactly, `Some(false)` = ignore them.
    pub diacritics: Option<bool>,
    pub wildcards: Option<bool>,
    pub whole_filename: Option<bool>,
    /// `ascii:` = fold ASCII letters only.
    pub ascii: Option<bool>,
    pub kind: Option<KindOnly>,
}

impl Mods {
    /// `self` refined by `inner` (inner settings win).
    #[must_use]
    pub fn merge(self, inner: Self) -> Self {
        Self {
            case: inner.case.or(self.case),
            whole_word: inner.whole_word.or(self.whole_word),
            path: inner.path.or(self.path),
            regex: inner.regex.or(self.regex),
            diacritics: inner.diacritics.or(self.diacritics),
            wildcards: inner.wildcards.or(self.wildcards),
            whole_filename: inner.whole_filename.or(self.whole_filename),
            ascii: inner.ascii.or(self.ascii),
            kind: inner.kind.or(self.kind),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn write_prefix(&self, out: &mut String) {
        let pairs: [(Option<bool>, &str, &str); 8] = [
            (self.case, "case:", "nocase:"),
            (self.whole_word, "ww:", "noww:"),
            (self.path, "path:", "nopath:"),
            (self.regex, "regex:", "noregex:"),
            (self.diacritics, "diacritics:", "nodiacritics:"),
            (self.wildcards, "wildcards:", "nowildcards:"),
            (self.whole_filename, "wfn:", "nowfn:"),
            (self.ascii, "ascii:", "utf8:"),
        ];
        for (v, on, off) in pairs {
            match v {
                Some(true) => out.push_str(on),
                Some(false) => out.push_str(off),
                None => {}
            }
        }
        match self.kind {
            Some(KindOnly::Files) => out.push_str("file:"),
            Some(KindOnly::Folders) => out.push_str("folder:"),
            Some(KindOnly::Any) => out.push_str("nofileonly:"),
            None => {}
        }
    }
}

/// A node of the query tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// All children must match (space-separated terms). Empty = match everything.
    And(Vec<Node>),
    /// Any child may match (`|`).
    Or(Vec<Node>),
    /// `!term`.
    Not(Box<Node>),
    /// Plain text (substring, wildcard, regex … depending on modifiers and options).
    Text { text: String, mods: Mods },
    /// `name:argument` with a known function name.
    Func {
        name: String,
        arg: String,
        mods: Mods,
    },
}

impl Node {
    /// The empty query (matches everything).
    #[must_use]
    pub fn all() -> Self {
        Self::And(Vec::new())
    }
}

/// True if `s` must be quoted to survive a print/parse round trip.
fn needs_quotes(s: &str) -> bool {
    s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || matches!(c, '|' | '<' | '>' | '!' | '"' | ':'))
}

/// Closes a bracket. A function with an empty argument right before `>` would otherwise read the
/// `>` as a comparison operator, so a space separates them.
fn close_bracket(out: &mut String) {
    if out.ends_with(':') {
        out.push(' ');
    }
    out.push('>');
}

fn write_quoted(out: &mut String, s: &str) {
    if needs_quotes(s) {
        let _ = write!(out, "\"{s}\"");
    } else {
        out.push_str(s);
    }
}

fn write_node(node: &Node, out: &mut String, parent_is_or: bool) {
    match node {
        Node::And(children) => {
            let bracket = parent_is_or || children.is_empty();
            if bracket {
                out.push('<');
            }
            for (i, c) in children.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                write_node(c, out, false);
            }
            if bracket {
                close_bracket(out);
            }
        }
        Node::Or(children) => {
            // OR binds tighter than AND, so an OR child of an AND needs no brackets; nested
            // ORs are bracketed to keep the tree shape.
            if parent_is_or {
                out.push('<');
            }
            for (i, c) in children.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                write_node(c, out, true);
            }
            if parent_is_or {
                close_bracket(out);
            }
        }
        Node::Not(child) => {
            out.push('!');
            match child.as_ref() {
                Node::And(_) | Node::Or(_) => {
                    out.push('<');
                    write_node(child, out, false);
                    close_bracket(out);
                }
                other => write_node(other, out, false),
            }
        }
        Node::Text { text, mods } => {
            mods.write_prefix(out);
            if !(text.is_empty() && !mods.is_empty()) {
                write_quoted(out, text);
            }
        }
        Node::Func { name, arg, mods } => {
            mods.write_prefix(out);
            out.push_str(name);
            out.push(':');
            if needs_quotes(arg) && !arg.is_empty() {
                let _ = write!(out, "\"{arg}\"");
            } else {
                out.push_str(arg);
            }
        }
    }
}

impl fmt::Display for Node {
    /// Prints the canonical query string; parsing it yields an equal tree.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = String::new();
        match self {
            Self::And(children) => {
                for (i, c) in children.iter().enumerate() {
                    if i > 0 {
                        s.push(' ');
                    }
                    write_node(c, &mut s, false);
                }
            }
            other => write_node(other, &mut s, false),
        }
        f.write_str(&s)
    }
}
