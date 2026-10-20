//! Lexer and parser for the Everything-compatible search syntax.
//!
//! The parser never fails: unbalanced brackets are closed implicitly, stray `>` are ignored, and
//! unknown `name:` prefixes are plain text (file names may contain `:`). Errors in function
//! arguments are reported later, when the query is compiled.

use std::collections::HashSet;

use super::ast::{KindOnly, Mods, Node};

/// Maximum bracket nesting; deeper `<` are treated as plain grouping-free text boundaries.
const MAX_DEPTH: usize = 128;

/// What a `name:` prefix means.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Prefix {
    Modifier(fn(&mut Mods)),
    /// A function (or macro) with its canonical name.
    Function(&'static str),
}

/// Canonical function names and their aliases.
pub(crate) const FUNCTIONS: &[(&str, &[&str])] = &[
    ("size", &["size"]),
    ("dm", &["dm", "datemodified"]),
    ("dc", &["dc", "datecreated"]),
    ("da", &["da", "dateaccessed"]),
    ("dch", &["dch", "datechanged"]),
    ("dr", &["dr", "daterun"]),
    ("rc", &["rc", "recentchange", "daterecentlychanged"]),
    ("ext", &["ext"]),
    ("type", &["type"]),
    ("parent", &["parent", "infolder", "nosubfolders"]),
    ("depth", &["depth", "parents"]),
    ("len", &["len"]),
    ("dupe", &["dupe"]),
    ("sizedupe", &["sizedupe"]),
    ("namepartdupe", &["namepartdupe"]),
    ("dmdupe", &["dmdupe"]),
    ("dcdupe", &["dcdupe"]),
    ("extdupe", &["extdupe"]),
    ("empty", &["empty"]),
    ("child", &["child"]),
    ("childfile", &["childfile"]),
    ("childfolder", &["childfolder"]),
    ("childcount", &["childcount"]),
    ("childfilecount", &["childfilecount"]),
    ("childfoldercount", &["childfoldercount"]),
    ("startwith", &["startwith"]),
    ("endwith", &["endwith"]),
    ("root", &["root"]),
    ("content", &["content"]),
    ("attrib", &["attrib", "attributes"]),
    ("runcount", &["runcount"]),
    ("inode", &["inode", "frn"]),
    ("fsi", &["fsi"]),
    ("perm", &["perm"]),
    ("owner", &["owner"]),
    ("group", &["group"]),
    ("uid", &["uid"]),
    ("gid", &["gid"]),
    ("symlink", &["symlink"]),
    ("hidden", &["hidden"]),
    ("fstype", &["fstype"]),
    ("volume", &["volume"]),
    ("tag", &["tag"]),
    // Built-in macros.
    ("audio", &["audio"]),
    ("zip", &["zip"]),
    ("doc", &["doc"]),
    ("exe", &["exe"]),
    ("pic", &["pic"]),
    ("video", &["video"]),
    // Recognized but not evaluable yet (see docs/TODO.md).
    ("width", &["width"]),
    ("height", &["height"]),
    ("dimensions", &["dimensions"]),
    ("orientation", &["orientation"]),
    ("bitdepth", &["bitdepth"]),
    ("album", &["album"]),
    ("artist", &["artist"]),
    ("genre", &["genre"]),
    ("title", &["title"]),
    ("track", &["track"]),
    ("comment", &["comment"]),
    ("year", &["year"]),
    ("length", &["length"]),
    ("filelist", &["filelist"]),
    ("filelistfilename", &["filelistfilename"]),
    ("shell", &["shell"]),
];

fn modifier(name: &str) -> Option<fn(&mut Mods)> {
    let f: fn(&mut Mods) = match name {
        "case" => |m| m.case = Some(true),
        "nocase" => |m| m.case = Some(false),
        "ww" | "wholeword" => |m| m.whole_word = Some(true),
        "noww" | "nowholeword" => |m| m.whole_word = Some(false),
        "path" => |m| m.path = Some(true),
        "nopath" => |m| m.path = Some(false),
        "regex" => |m| m.regex = Some(true),
        "noregex" => |m| m.regex = Some(false),
        "diacritics" => |m| m.diacritics = Some(true),
        "nodiacritics" => |m| m.diacritics = Some(false),
        "wildcards" => |m| m.wildcards = Some(true),
        "nowildcards" => |m| m.wildcards = Some(false),
        "wfn" | "wholefilename" => |m| m.whole_filename = Some(true),
        "nowfn" | "nowholefilename" => |m| m.whole_filename = Some(false),
        "ascii" => |m| m.ascii = Some(true),
        "utf8" | "noascii" => |m| m.ascii = Some(false),
        "file" | "files" => |m| m.kind = Some(KindOnly::Files),
        "folder" | "folders" => |m| m.kind = Some(KindOnly::Folders),
        "nofileonly" | "nofolderonly" => |m| m.kind = Some(KindOnly::Any),
        _ => return None,
    };
    Some(f)
}

/// Parser configuration.
#[derive(Clone, Debug, Default)]
pub struct ParseOptions {
    /// Names of user-defined macros (recognized like functions).
    pub macros: HashSet<String>,
}

impl ParseOptions {
    fn prefix(&self, raw: &str) -> Option<Prefix> {
        let lower = raw.to_ascii_lowercase();
        if let Some(m) = modifier(&lower) {
            return Some(Prefix::Modifier(m));
        }
        for &(canonical, aliases) in FUNCTIONS {
            if aliases.contains(&lower.as_str()) {
                return Some(Prefix::Function(canonical));
            }
        }
        if self.macros.contains(&lower) {
            // User macro names are not 'static; they are stored in the node by name.
            return Some(Prefix::Function(""));
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Open,
    Close,
    Or,
    Not,
    /// `prefix` = byte length of the leading part that was not quoted; only `:` separators in
    /// that part can introduce modifiers and functions.
    Word {
        text: String,
        prefix: usize,
    },
}

#[derive(Clone, Debug)]
struct Lexed {
    tok: Tok,
    space_before: bool,
}

fn lex(input: &str, opts: &ParseOptions) -> Vec<Lexed> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut space_before = false;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            space_before = true;
            i += 1;
            continue;
        }
        let tok = match c {
            '<' => Some(Tok::Open),
            '>' => Some(Tok::Close),
            '|' => Some(Tok::Or),
            '!' => Some(Tok::Not),
            _ => None,
        };
        if let Some(tok) = tok {
            out.push(Lexed { tok, space_before });
            space_before = false;
            i += 1;
            continue;
        }
        // A word.
        let mut text = String::new();
        let mut in_quotes = false;
        let mut prefix: Option<usize> = None;
        let mut segment_start = 0usize;
        let mut after_func_colon = false;
        while i < chars.len() {
            let c = chars[i];
            if c == '"' {
                in_quotes = !in_quotes;
                prefix.get_or_insert(text.len());
                after_func_colon = false;
                i += 1;
                continue;
            }
            if !in_quotes {
                if after_func_colon && matches!(c, '<' | '>' | '=') {
                    text.push(c);
                    i += 1;
                    continue;
                }
                if c.is_whitespace() || matches!(c, '|' | '<' | '>') {
                    break;
                }
            }
            after_func_colon = false;
            text.push(c);
            if c == ':' && prefix.is_none() {
                let segment = &text[segment_start..text.len() - 1];
                after_func_colon = matches!(opts.prefix(segment), Some(Prefix::Function(_)));
                segment_start = text.len();
            }
            i += 1;
        }
        let prefix = prefix.unwrap_or(text.len());
        out.push(Lexed {
            tok: Tok::Word { text, prefix },
            space_before,
        });
        space_before = false;
    }
    out
}

struct Parser<'a> {
    toks: Vec<Lexed>,
    pos: usize,
    depth: usize,
    opts: &'a ParseOptions,
}

/// Parses a query string into a normalized tree.
#[must_use]
pub fn parse(input: &str, opts: &ParseOptions) -> Node {
    let mut p = Parser {
        toks: lex(input, opts),
        pos: 0,
        depth: 0,
        opts,
    };
    let mut items = Vec::new();
    while p.pos < p.toks.len() {
        if p.peek() == Some(&Tok::Close) {
            p.pos += 1; // stray '>'
            continue;
        }
        if let Some(n) = p.parse_or() {
            items.push(n);
        }
    }
    normalize(Node::And(items))
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|l| &l.tok)
    }

    fn parse_and_until_close(&mut self) -> Node {
        let mut items = Vec::new();
        while let Some(t) = self.peek() {
            if *t == Tok::Close {
                self.pos += 1;
                break;
            }
            if let Some(n) = self.parse_or() {
                items.push(n);
            }
        }
        Node::And(items)
    }

    fn parse_or(&mut self) -> Option<Node> {
        let mut alts = Vec::new();
        if let Some(n) = self.parse_unary() {
            alts.push(n);
        }
        while self.peek() == Some(&Tok::Or) {
            self.pos += 1;
            if let Some(n) = self.parse_unary() {
                alts.push(n);
            }
        }
        match alts.len() {
            0 => None,
            1 => alts.pop(),
            _ => Some(Node::Or(alts)),
        }
    }

    fn parse_unary(&mut self) -> Option<Node> {
        match self.peek()? {
            Tok::Not => {
                self.pos += 1;
                self.parse_unary().map(|n| Node::Not(Box::new(n)))
            }
            Tok::Or => None,
            Tok::Close => None,
            Tok::Open => {
                self.pos += 1;
                if self.depth >= MAX_DEPTH {
                    return None;
                }
                self.depth += 1;
                let n = self.parse_and_until_close();
                self.depth -= 1;
                Some(n)
            }
            Tok::Word { .. } => {
                let Some(Lexed {
                    tok: Tok::Word { text, prefix },
                    ..
                }) = self.toks.get(self.pos).cloned()
                else {
                    return None;
                };
                self.pos += 1;
                Some(self.word(&text, prefix))
            }
        }
    }

    /// Interprets one word: modifiers, a function call, or plain text. Only `:` within the
    /// first `prefix` bytes (the unquoted start of the word) separate modifiers and functions.
    fn word(&mut self, text: &str, prefix: usize) -> Node {
        let mut mods = Mods::default();
        let mut at = 0usize;
        while let Some(colon) = text[at..prefix].find(':').map(|c| c + at) {
            let name = &text[at..colon];
            match self.opts.prefix(name) {
                Some(Prefix::Modifier(apply)) => {
                    apply(&mut mods);
                    at = colon + 1;
                }
                Some(Prefix::Function(canonical)) => {
                    let name = if canonical.is_empty() {
                        name.to_ascii_lowercase()
                    } else {
                        canonical.to_owned()
                    };
                    return Node::Func {
                        name,
                        arg: text[colon + 1..].to_owned(),
                        mods,
                    };
                }
                None => break,
            }
        }
        let rest = &text[at..];
        if rest.is_empty() && !mods.is_empty() {
            // Modifiers directly followed by a group or `!` apply to it.
            let next_adjacent = self.toks.get(self.pos).is_some_and(|l| {
                !l.space_before && matches!(l.tok, Tok::Open | Tok::Not | Tok::Word { .. })
            });
            if next_adjacent && let Some(n) = self.parse_unary() {
                return apply_mods(n, mods);
            }
        }
        Node::Text {
            text: rest.to_owned(),
            mods,
        }
    }
}

/// Pushes modifiers down to every leaf (leaf modifiers win).
fn apply_mods(node: Node, outer: Mods) -> Node {
    match node {
        Node::And(c) => Node::And(c.into_iter().map(|n| apply_mods(n, outer)).collect()),
        Node::Or(c) => Node::Or(c.into_iter().map(|n| apply_mods(n, outer)).collect()),
        Node::Not(c) => Node::Not(Box::new(apply_mods(*c, outer))),
        Node::Text { text, mods } => Node::Text {
            text,
            mods: outer.merge(mods),
        },
        Node::Func { name, arg, mods } => Node::Func {
            name,
            arg,
            mods: outer.merge(mods),
        },
    }
}

/// Flattens nested AND/OR and unwraps single-child groups.
#[must_use]
pub fn normalize(node: Node) -> Node {
    match node {
        Node::And(children) => {
            let mut flat = Vec::new();
            for c in children {
                match normalize(c) {
                    Node::And(inner) if !inner.is_empty() => flat.extend(inner),
                    other => flat.push(other),
                }
            }
            if flat.len() == 1 {
                flat.pop().unwrap_or_else(Node::all)
            } else {
                Node::And(flat)
            }
        }
        Node::Or(children) => {
            let mut flat = Vec::new();
            for c in children {
                match normalize(c) {
                    Node::Or(inner) => flat.extend(inner),
                    other => flat.push(other),
                }
            }
            if flat.len() == 1 {
                flat.pop().unwrap_or_else(Node::all)
            } else {
                Node::Or(flat)
            }
        }
        Node::Not(c) => Node::Not(Box::new(normalize(*c))),
        leaf => leaf,
    }
}
