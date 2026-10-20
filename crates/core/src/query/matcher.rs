//! Text matching: substring, prefix/suffix/exact, whole word and regex/wildcards, with case and
//! diacritics handling.

use memchr::memmem;
use regex::bytes::{Regex, RegexBuilder};
use smallvec::SmallVec;

use crate::fold;

/// How a text term matches.
#[derive(Clone, Debug)]
pub enum TextMode {
    Substring,
    Prefix,
    Suffix,
    Exact,
    WholeWord,
    /// Wildcards made of literals and `*` only, matched piece by piece.
    Glob(Box<Glob>),
    /// Regular expression (from `regex:` or wildcards containing `?`).
    Regex(Regex),
}

/// A wildcard pattern without `?`: literal pieces separated by `*`, normalized like haystacks.
/// Matching places each piece at its leftmost occurrence after the previous one, which is exact
/// for `*`-only patterns.
#[derive(Clone, Debug)]
pub struct Glob {
    pieces: Vec<Vec<u8>>,
    finders: Vec<memmem::Finder<'static>>,
    /// Whole-name match: the first piece is a prefix and the last a suffix.
    anchored: bool,
}

impl Glob {
    fn new(pattern: &str, case: bool, diacritics: bool, ascii_only: bool, anchored: bool) -> Self {
        let pieces: Vec<Vec<u8>> = pattern
            .split('*')
            .map(|p| {
                let mut v = Vec::with_capacity(p.len());
                normalize_into(p.as_bytes(), case, diacritics, ascii_only, &mut v);
                v
            })
            .collect();
        let finders = pieces
            .iter()
            .map(|p| memmem::Finder::new(p).into_owned())
            .collect();
        Self {
            pieces,
            finders,
            anchored,
        }
    }

    /// The longest piece (a substring every match contains).
    fn longest(&self) -> &[u8] {
        self.pieces
            .iter()
            .max_by_key(|p| p.len())
            .map_or(&[][..], Vec::as_slice)
    }

    /// Matches a normalized haystack, reporting where each non-empty piece landed.
    fn walk(&self, hay: &[u8], mut found: impl FnMut(usize, usize)) -> bool {
        let n = self.pieces.len();
        let (mut lo, mut hi) = (0, hay.len());
        let mut middle = 0..n;
        if self.anchored {
            let (first, last) = (&self.pieces[0], &self.pieces[n - 1]);
            // `split` always yields at least two pieces here: the pattern contains a `*`.
            if n < 2
                || hay.len() < first.len() + last.len()
                || !hay.starts_with(first)
                || !hay.ends_with(last)
            {
                return n < 2 && hay == first.as_slice();
            }
            lo = first.len();
            hi = hay.len() - last.len();
            middle = 1..n - 1;
        }
        let mut spans: SmallVec<[(usize, usize); 4]> = SmallVec::new();
        for i in middle {
            let p = &self.pieces[i];
            if p.is_empty() {
                continue;
            }
            match self.finders[i].find(&hay[lo..hi]) {
                Some(at) => {
                    spans.push((lo + at, lo + at + p.len()));
                    lo += at + p.len();
                }
                None => return false,
            }
        }
        if self.anchored {
            let (first, last) = (&self.pieces[0], &self.pieces[n - 1]);
            if !first.is_empty() {
                found(0, first.len());
            }
            spans.iter().for_each(|&(s, e)| found(s, e));
            if !last.is_empty() {
                found(hay.len() - last.len(), hay.len());
            }
        } else {
            spans.iter().for_each(|&(s, e)| found(s, e));
        }
        true
    }
}

/// A compiled text matcher.
#[derive(Clone, Debug)]
pub struct TextMatch {
    pub mode: TextMode,
    /// Case-sensitive.
    pub case: bool,
    /// Match diacritics exactly (`false` = ignore them).
    pub diacritics: bool,
    /// `ascii:`: fold ASCII letters only, no Unicode normalization.
    pub ascii_only: bool,
    /// The needle as typed (for regex: the pattern).
    pub raw: String,
    /// The needle normalized like haystacks are (folded / stripped as the flags say).
    pub needle: Vec<u8>,
    pub needle_ascii: bool,
    finder: memmem::Finder<'static>,
    /// ASCII case-insensitive searcher for raw arenas (case-insensitive ASCII needles only).
    ci: Option<aho_corasick::AhoCorasick>,
    /// For regexes: the same pattern with `^`/`$` matching at NUL, for scanning a whole name
    /// arena at once. `None` when the pattern has anchors that would behave differently there.
    arena_re: Option<Regex>,
}

/// Translates Everything wildcards (`*`, `?`) into a regex pattern.
#[must_use]
pub fn wildcard_to_regex(pattern: &str, anchored: bool) -> String {
    let mut out = String::with_capacity(pattern.len() * 2 + 8);
    out.push_str("(?s)");
    if anchored {
        out.push('^');
    }
    for c in pattern.chars() {
        match c {
            // `*` spans any bytes, including ones that aren’t valid UTF-8.
            '*' => out.push_str("(?-u:.)*"),
            '?' => out.push('.'),
            c => out.push_str(&regex::escape(c.encode_utf8(&mut [0u8; 4]))),
        }
    }
    if anchored {
        out.push('$');
    }
    out
}

impl TextMatch {
    /// A non-regex matcher.
    #[must_use]
    pub fn literal(
        text: &str,
        mode: TextMode,
        case: bool,
        diacritics: bool,
        ascii_only: bool,
    ) -> Self {
        let mut needle = Vec::with_capacity(text.len());
        normalize_into(text.as_bytes(), case, diacritics, ascii_only, &mut needle);
        Self::with_needle(text, needle, mode, case, diacritics, ascii_only)
    }

    /// A wildcard matcher for a pattern containing `*` but no `?`.
    #[must_use]
    pub fn glob(
        pattern: &str,
        anchored: bool,
        case: bool,
        diacritics: bool,
        ascii_only: bool,
    ) -> Self {
        let glob = Glob::new(pattern, case, diacritics, ascii_only, anchored);
        let needle = glob.longest().to_vec();
        let all_ascii = glob.pieces.iter().all(|p| p.is_ascii());
        let mut m = Self::with_needle(
            pattern,
            needle,
            TextMode::Glob(Box::new(glob)),
            case,
            diacritics,
            ascii_only,
        );
        // Every piece must be ASCII for an ASCII name to match.
        m.needle_ascii = all_ascii;
        m
    }

    fn with_needle(
        text: &str,
        needle: Vec<u8>,
        mode: TextMode,
        case: bool,
        diacritics: bool,
        ascii_only: bool,
    ) -> Self {
        let needle_ascii = needle.is_ascii();
        let finder = memmem::Finder::new(&needle).into_owned();
        let ci = (!case && needle_ascii && !needle.is_empty())
            .then(|| {
                aho_corasick::AhoCorasick::builder()
                    .ascii_case_insensitive(true)
                    .build([&needle])
                    .ok()
            })
            .flatten();
        Self {
            mode,
            case,
            diacritics,
            ascii_only,
            raw: text.to_owned(),
            needle,
            needle_ascii,
            finder,
            ci,
            arena_re: None,
        }
    }

    /// Finds the next occurrence of the (ASCII) needle in raw bytes at or after `start`,
    /// honouring case sensitivity. Only meaningful when `needle_ascii`.
    #[inline]
    pub(crate) fn find_raw(&self, hay: &[u8], start: usize) -> Option<usize> {
        let rest = hay.get(start..)?;
        match &self.ci {
            Some(ac) => ac.find(rest).map(|m| m.start() + start),
            None => self.finder.find(rest).map(|i| i + start),
        }
    }

    /// A regex matcher (`pattern` is a regex; case-insensitive unless `case`).
    pub fn regex(pattern: &str, case: bool, diacritics: bool) -> Result<Self, String> {
        let re = RegexBuilder::new(pattern)
            .case_insensitive(!case)
            .size_limit(32 << 20)
            .build()
            .map_err(|e| e.to_string())?;
        let mut m = Self::literal("", TextMode::Substring, case, diacritics, false);
        m.mode = TextMode::Regex(re);
        m.raw = pattern.to_owned();
        m.arena_re = arena_regex(pattern, case);
        Ok(m)
    }

    /// The arena variant of a regex (see `arena_re`).
    pub(crate) fn arena_regex(&self) -> Option<&Regex> {
        self.arena_re.as_ref()
    }

    #[must_use]
    pub fn is_regex(&self) -> bool {
        matches!(self.mode, TextMode::Regex(_))
    }

    /// True if an empty needle (matches everything).
    #[must_use]
    pub fn matches_all(&self) -> bool {
        !self.is_regex() && self.needle.is_empty() && !matches!(self.mode, TextMode::Exact)
    }

    /// True if haystacks for this matcher are normalized (folded and/or stripped), so the
    /// caller must normalize non-ASCII names before calling [`Self::is_match_normalized`].
    #[must_use]
    pub fn folds_case(&self) -> bool {
        !self.case && !self.is_regex()
    }

    /// Matches a raw name or path.
    #[must_use]
    pub fn is_match(&self, hay: &[u8]) -> bool {
        if hay.is_ascii() || self.ascii_only {
            if let TextMode::Regex(re) = &self.mode {
                return re.is_match(hay);
            }
            if self.case {
                return self.is_match_normalized(hay);
            }
            // ASCII folding without allocating for short names.
            let mut buf = [0u8; 256];
            if hay.len() <= buf.len() {
                for (d, s) in buf.iter_mut().zip(hay) {
                    *d = s.to_ascii_lowercase();
                }
                return self.is_match_normalized(&buf[..hay.len()]);
            }
            return self.is_match_normalized(&hay.to_ascii_lowercase());
        }
        let mut norm = Vec::with_capacity(hay.len() + 8);
        self.normalize_hay(hay, &mut norm);
        self.is_match_normalized(&norm)
    }

    /// Normalizes a haystack the way this matcher expects.
    pub fn normalize_hay(&self, hay: &[u8], out: &mut Vec<u8>) {
        let fold_case = !self.case && !self.is_regex();
        normalize_into(hay, !fold_case, self.diacritics, self.ascii_only, out);
    }

    /// Matches an already-normalized haystack.
    #[must_use]
    pub fn is_match_normalized(&self, hay: &[u8]) -> bool {
        match &self.mode {
            TextMode::Substring => self.finder.find(hay).is_some(),
            TextMode::Prefix => hay.starts_with(&self.needle),
            TextMode::Suffix => hay.ends_with(&self.needle),
            TextMode::Exact => hay == self.needle.as_slice(),
            TextMode::WholeWord => self.find_word(hay).is_some(),
            TextMode::Glob(g) => g.walk(hay, |_, _| {}),
            TextMode::Regex(re) => re.is_match(hay),
        }
    }

    fn find_word(&self, hay: &[u8]) -> Option<(usize, usize)> {
        if self.needle.is_empty() {
            return Some((0, 0));
        }
        let mut from = 0;
        while let Some(i) = self.finder.find(&hay[from..]).map(|i| i + from) {
            let end = i + self.needle.len();
            if is_word_boundary_before(hay, i) && is_word_boundary_after(hay, end) {
                return Some((i, end));
            }
            from = i + 1;
        }
        None
    }

    /// Byte ranges of matches in a raw name (for highlighting). Best effort: names whose
    /// normalization changes their length (decomposed accents) may get no highlight.
    #[must_use]
    pub fn highlight(&self, raw: &[u8]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        if let TextMode::Regex(re) = &self.mode {
            for m in re.find_iter(raw) {
                if m.start() != m.end() {
                    out.push((m.start(), m.end()));
                }
            }
            return out;
        }
        if self.needle.is_empty() {
            return out;
        }
        let mut norm = Vec::with_capacity(raw.len());
        self.normalize_hay(raw, &mut norm);
        if norm.len() != raw.len() {
            // Map through characters: simple folding is 1:1 per character, so walk both.
            return self.highlight_by_chars(raw);
        }
        let ranges: Vec<(usize, usize)> = match self.mode {
            TextMode::Prefix => {
                if norm.starts_with(&self.needle) {
                    vec![(0, self.needle.len())]
                } else {
                    vec![]
                }
            }
            TextMode::Suffix => {
                if norm.ends_with(&self.needle) {
                    vec![(norm.len() - self.needle.len(), norm.len())]
                } else {
                    vec![]
                }
            }
            TextMode::Exact => {
                if norm == self.needle {
                    vec![(0, norm.len())]
                } else {
                    vec![]
                }
            }
            TextMode::Glob(ref g) => {
                let mut v = Vec::new();
                if !g.walk(&norm, |s, e| v.push((s, e))) {
                    v.clear();
                }
                v
            }
            _ => self
                .finder
                .find_iter(&norm)
                .map(|i| (i, i + self.needle.len()))
                .filter(|&(s, e)| {
                    !matches!(self.mode, TextMode::WholeWord)
                        || (is_word_boundary_before(&norm, s) && is_word_boundary_after(&norm, e))
                })
                .collect(),
        };
        out.extend(ranges);
        out
    }

    fn highlight_by_chars(&self, raw: &[u8]) -> Vec<(usize, usize)> {
        let Ok(s) = std::str::from_utf8(raw) else {
            return Vec::new();
        };
        let Ok(needle) = std::str::from_utf8(&self.needle) else {
            return Vec::new();
        };
        let needle: Vec<char> = needle.chars().collect();
        let chars: Vec<(usize, char)> = s.char_indices().collect();
        let conv = |c: char| if self.case { c } else { fold::fold_char(c) };
        let mut out = Vec::new();
        let mut i = 0;
        while i + needle.len() <= chars.len() {
            if needle
                .iter()
                .enumerate()
                .all(|(k, &n)| conv(chars[i + k].1) == n)
            {
                let start = chars[i].0;
                let end = chars.get(i + needle.len()).map_or(raw.len(), |&(b, _)| b);
                out.push((start, end));
                i += needle.len().max(1);
            } else {
                i += 1;
            }
        }
        out
    }
}

/// Builds the arena variant of a user regex: `^` and `$` match next to the NUL separators of a
/// name arena, so one search covers a whole page. Only patterns whose anchors all become line
/// anchors qualify: `\A`, `\z`, `(?-m)` and an explicit `(?m)` (which anchors at `\n` in a
/// single name) behave differently in an arena, so they keep per-name matching.
fn arena_regex(pattern: &str, case: bool) -> Option<Regex> {
    use regex_syntax::hir::{Hir, HirKind, Look};
    fn looks(h: &Hir, out: &mut Vec<Look>) {
        match h.kind() {
            HirKind::Look(l) => out.push(*l),
            HirKind::Repetition(r) => looks(&r.sub, out),
            HirKind::Capture(c) => looks(&c.sub, out),
            HirKind::Concat(v) | HirKind::Alternation(v) => v.iter().for_each(|x| looks(x, out)),
            HirKind::Empty | HirKind::Literal(_) | HirKind::Class(_) => {}
        }
    }
    let parse = |multi_line| {
        regex_syntax::ParserBuilder::new()
            .multi_line(multi_line)
            .case_insensitive(!case)
            .utf8(false)
            .build()
            .parse(pattern)
            .ok()
    };
    let (mut as_lines, mut as_name) = (Vec::new(), Vec::new());
    looks(&parse(true)?, &mut as_lines);
    looks(&parse(false)?, &mut as_name);
    let absolute = |l: &Look| matches!(l, Look::Start | Look::End);
    let line = |l: &Look| {
        matches!(
            l,
            Look::StartLF | Look::EndLF | Look::StartCRLF | Look::EndCRLF
        )
    };
    if as_lines.iter().any(absolute) || as_name.iter().any(line) {
        return None;
    }
    RegexBuilder::new(pattern)
        .case_insensitive(!case)
        .multi_line(true)
        .line_terminator(0)
        .size_limit(32 << 20)
        .build()
        .ok()
}

/// Normalizes `text`: optional case folding, NFC (or diacritics stripping) for non-ASCII.
pub(crate) fn normalize_into(
    text: &[u8],
    case_sensitive: bool,
    diacritics: bool,
    ascii_only: bool,
    out: &mut Vec<u8>,
) {
    if ascii_only || text.is_ascii() {
        if case_sensitive {
            out.extend_from_slice(text);
        } else {
            out.extend(text.iter().map(u8::to_ascii_lowercase));
        }
        return;
    }
    if diacritics {
        fold::normalize_into(text, !case_sensitive, out);
    } else {
        fold::strip_diacritics_into(text, !case_sensitive, out);
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_word_boundary_before(hay: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = String::from_utf8_lossy(&hay[i.saturating_sub(4)..i])
        .chars()
        .last();
    !prev.is_some_and(is_word_char)
}

fn is_word_boundary_after(hay: &[u8], end: usize) -> bool {
    if end >= hay.len() {
        return true;
    }
    let next = String::from_utf8_lossy(&hay[end..(end + 4).min(hay.len())])
        .chars()
        .next();
    !next.is_some_and(is_word_char)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(t: &str) -> TextMatch {
        TextMatch::literal(t, TextMode::Substring, false, true, false)
    }

    #[test]
    fn substring_case_insensitive_unicode() {
        assert!(sub("report").is_match(b"Annual_REPORT_2023.pdf"));
        assert!(sub("ärger").is_match("ÄRGER.txt".as_bytes()));
        assert!(sub("σ").is_match("ΟΔΟΣ".as_bytes()));
        assert!(sub("k").is_match("\u{212A}elvin".as_bytes()));
        assert!(!sub("cafe").is_match("café".as_bytes()));
        assert!(
            sub("café").is_match("CAFE\u{301}.txt".as_bytes()),
            "NFC normalization"
        );
        assert!(sub("\u{FF}").is_match("\u{FF}".as_bytes()));
        assert!(sub("x").is_match(b"\xFFx"));
    }

    #[test]
    fn diacritics_insensitive() {
        let m = TextMatch::literal("cafe", TextMode::Substring, false, false, false);
        assert!(m.is_match("Café".as_bytes()));
        assert!(m.is_match("CAFE\u{301}".as_bytes()));
        let m = TextMatch::literal("crème", TextMode::Substring, false, false, false);
        assert!(m.is_match(b"CREME brulee"));
    }

    #[test]
    fn modes() {
        let m = TextMatch::literal("rep", TextMode::Prefix, false, true, false);
        assert!(m.is_match(b"Report.txt") && !m.is_match(b"myreport"));
        let m = TextMatch::literal(".TXT", TextMode::Suffix, false, true, false);
        assert!(m.is_match(b"a.txt") && !m.is_match(b"a.txt.bak"));
        let m = TextMatch::literal("foo", TextMode::WholeWord, false, true, false);
        assert!(m.is_match(b"a foo b") && m.is_match(b"foo-bar") && !m.is_match(b"foobar"));
        assert!(m.is_match(b"foobar foo"));
        let m = TextMatch::literal("Foo", TextMode::Substring, true, true, false);
        assert!(m.is_match(b"xFoox") && !m.is_match(b"xfoox"));
        let m = TextMatch::regex(&wildcard_to_regex("*.tx?", true), false, true).unwrap();
        assert!(m.is_match(b"notes.TXT") && !m.is_match(b"notes.txt.gz"));
        let m = TextMatch::regex(&wildcard_to_regex("re*rt", false), false, true).unwrap();
        assert!(m.is_match(b"my-report.pdf"));
        assert!(TextMatch::regex("(", false, true).is_err());
    }

    #[test]
    fn globs() {
        let g = |p: &str, anchored| TextMatch::glob(p, anchored, false, true, false);
        let m = g("*.pdf", true);
        assert!(m.is_match(b"Report.PDF") && !m.is_match(b"a.pdf.bak") && m.is_match(b".pdf"));
        let m = g("re*rt*", true);
        assert!(m.is_match(b"Report.txt") && !m.is_match(b"my-report"));
        assert!(m.is_match(b"rert") && !m.is_match(b"ret"));
        assert!(g("a*a", true).is_match(b"aa") && !g("a*a", true).is_match(b"a"));
        assert!(g("*", true).is_match(b"") && g("*", true).matches_all());
        let m = g("re*rt", false);
        assert!(m.is_match(b"my-report.pdf") && !m.is_match(b"tr-er"));
        let m = g("*CAFÉ*", true);
        assert!(m.is_match("le café.txt".as_bytes()) && !m.is_match(b"cafe"));
        let m = TextMatch::glob("*cafe*", true, false, false, false);
        assert!(
            m.is_match("Café".as_bytes()),
            "diacritics-insensitive globs"
        );
        assert_eq!(g("*.pdf", true).highlight(b"a.PDF"), vec![(1, 5)]);
        assert_eq!(
            g("r*p*t", true).highlight(b"report"),
            vec![(0, 1), (2, 3), (5, 6)]
        );
        assert!(g("*.pdf", true).highlight(b"a.txt").is_empty());
    }

    #[test]
    fn arena_regexes() {
        let has_arena = |p: &str| TextMatch::regex(p, false, true).unwrap().arena_re.is_some();
        assert!(has_arena("^data_[0-9]+$"));
        assert!(has_arena(r"\bfoo\b"));
        assert!(!has_arena(r"\Adata"));
        assert!(!has_arena(r"data\z"));
        assert!(!has_arena("(?-m)^data"));
        assert!(!has_arena("(?m)^data"));
        let m = TextMatch::regex("^b.*c$", false, true).unwrap();
        let re = m.arena_re.unwrap();
        let arena = b"abc\0bxc\0b\0c\0";
        let hits: Vec<usize> = re.find_iter(arena).map(|x| x.start()).collect();
        assert_eq!(hits, vec![4]);
    }

    #[test]
    fn highlights() {
        assert_eq!(sub("rep").highlight(b"REPort rep"), vec![(0, 3), (7, 10)]);
        assert_eq!(sub("ärg").highlight("xÄRGER".as_bytes()), vec![(1, 5)]);
        let re = TextMatch::regex("o+", false, true).unwrap();
        assert_eq!(re.highlight(b"foo boo"), vec![(1, 3), (5, 7)]);
    }
}
