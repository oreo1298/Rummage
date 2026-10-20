//! Name patterns for excluding entries everywhere (`node_modules`, `.cache`, `*.o`).

/// Shell-style patterns matched against whole entry names: `*` matches any run of characters,
/// `?` one character; everything else matches itself, case-sensitively.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamePatterns {
    pats: Vec<Vec<u8>>,
}

impl NamePatterns {
    #[must_use]
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Self {
        Self {
            pats: patterns
                .iter()
                .map(|p| p.as_ref().as_bytes().to_vec())
                .filter(|p| !p.is_empty())
                .collect(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pats.is_empty()
    }

    /// True if `name` matches one of the patterns.
    #[must_use]
    pub fn matches(&self, name: &[u8]) -> bool {
        self.pats.iter().any(|p| glob(p, name))
    }

    /// True if any component of `relative` (a `/`-separated path) matches.
    #[must_use]
    pub fn matches_any_component(&self, relative: &[u8]) -> bool {
        !self.is_empty()
            && relative
                .split(|&b| b == b'/')
                .any(|c| !c.is_empty() && self.matches(c))
    }
}

/// Length of the character starting at `s[0]` (1 for bytes that aren’t valid UTF-8).
fn char_len(s: &[u8]) -> usize {
    let n = match s.first() {
        Some(&b) if b >= 0xF0 => 4,
        Some(&b) if b >= 0xE0 => 3,
        Some(&b) if b >= 0xC0 => 2,
        _ => 1,
    };
    if n > 1 && s.len() >= n && std::str::from_utf8(&s[..n]).is_ok() {
        n
    } else {
        1
    }
}

/// Matches with backtracking to the last `*` only (linear in practice).
fn glob(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        match p.get(pi) {
            Some(b'*') => {
                star = Some((pi, si));
                pi += 1;
            }
            Some(b'?') => {
                pi += 1;
                si += char_len(&s[si..]);
            }
            Some(&c) if c == s[si] => {
                pi += 1;
                si += 1;
            }
            _ => match star {
                Some((sp, ss)) => {
                    let next = ss + char_len(&s[ss..]);
                    star = Some((sp, next));
                    pi = sp + 1;
                    si = next;
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let p = NamePatterns::new(&["node_modules", "*.o", ".cache", "build-??", "a*b*c"]);
        assert!(p.matches(b"node_modules"));
        assert!(!p.matches(b"node_modules2"));
        assert!(p.matches(b"main.o") && p.matches(b".o"));
        assert!(!p.matches(b"main.oo"));
        assert!(!p.matches(b"build-x86"));
        assert!(p.matches(b"build-42"));
        assert!(p.matches("build-éé".as_bytes()), "? is one character");
        assert!(p.matches(b"aXXbYYc") && p.matches(b"abc") && !p.matches(b"acb"));
        assert!(p.matches_any_component(b"src/node_modules/x/y.js"));
        assert!(!p.matches_any_component(b"src/modules/x"));
        assert!(!NamePatterns::new::<&str>(&[]).matches_any_component(b"a/b"));
        assert!(NamePatterns::new(&["*"]).matches(b"\xff\xfe"));
    }
}
