//! Row selection for result lists of any size.
//!
//! A selection is a set of row ranges, so selecting all of five million rows costs nothing.
//! Small selections can be pinned to the selected files (by path) when the result changes
//! live, so the selection follows the files rather than the row numbers.

use std::collections::HashSet;

/// Selections with up to this many rows follow their files across live changes.
pub const PIN_LIMIT: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Set {
    /// Inclusive row ranges, sorted and disjoint.
    Rows(Vec<(u64, u64)>),
    /// Selected files by path.
    Paths(HashSet<Vec<u8>>),
}

/// How a click (or key) changes the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Select only this row.
    Replace,
    /// Toggle this row (Ctrl+click).
    Toggle,
    /// Select from the anchor to this row (Shift+click).
    Extend,
    /// Add the range from the anchor to this row (Ctrl+Shift+click).
    AddRange,
}

impl Mode {
    #[must_use]
    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Toggle,
            2 => Self::Extend,
            3 => Self::AddRange,
            _ => Self::Replace,
        }
    }
}

/// The selection of one result list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    set: Set,
    anchor: Option<u64>,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            set: Set::Rows(Vec::new()),
            anchor: None,
        }
    }
}

fn normalize(ranges: &mut Vec<(u64, u64)>) {
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for &(a, b) in ranges.iter() {
        match out.last_mut() {
            Some(last) if a <= last.1.saturating_add(1) => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    *ranges = out;
}

fn remove_row(ranges: &mut Vec<(u64, u64)>, row: u64) {
    let mut out = Vec::with_capacity(ranges.len() + 1);
    for &(a, b) in ranges.iter() {
        if row < a || row > b {
            out.push((a, b));
            continue;
        }
        if a < row {
            out.push((a, row - 1));
        }
        if row < b {
            out.push((row + 1, b));
        }
    }
    *ranges = out;
}

impl Selection {
    /// True if `row` (whose file is at `path`, if known) is selected.
    #[must_use]
    pub fn contains(&self, row: u64, path: Option<&[u8]>) -> bool {
        match &self.set {
            Set::Rows(r) => {
                let i = r.partition_point(|&(_, b)| b < row);
                r.get(i).is_some_and(|&(a, _)| a <= row)
            }
            Set::Paths(p) => path.is_some_and(|p2| p.contains(p2)),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        match &self.set {
            Set::Rows(r) => r.is_empty(),
            Set::Paths(p) => p.is_empty(),
        }
    }

    /// Number of selected rows (files, once pinned).
    #[must_use]
    pub fn count(&self) -> u64 {
        match &self.set {
            Set::Rows(r) => r.iter().map(|&(a, b)| b - a + 1).sum(),
            Set::Paths(p) => p.len() as u64,
        }
    }

    pub fn clear(&mut self) {
        self.set = Set::Rows(Vec::new());
        self.anchor = None;
    }

    /// Selects every row of a result with `total` rows.
    pub fn select_all(&mut self, total: u64) {
        self.set = Set::Rows(if total == 0 {
            Vec::new()
        } else {
            vec![(0, total - 1)]
        });
    }

    /// Applies a click on `row`. `path_of` gives the path of a (loaded) row, needed only when
    /// the selection is pinned to paths.
    pub fn click(&mut self, row: u64, mode: Mode, path_of: &dyn Fn(u64) -> Option<Vec<u8>>) {
        // A pinned selection stays pinned, except that a plain click starts over.
        if let Set::Paths(p) = &mut self.set {
            match mode {
                Mode::Replace => {
                    self.set = Set::Rows(vec![(row, row)]);
                    self.anchor = Some(row);
                }
                Mode::Toggle => {
                    if let Some(path) = path_of(row)
                        && !p.remove(&path)
                    {
                        p.insert(path);
                    }
                    self.anchor = Some(row);
                }
                Mode::Extend | Mode::AddRange => {
                    let anchor = self.anchor.unwrap_or(row);
                    for r in anchor.min(row)..=anchor.max(row) {
                        if let Some(path) = path_of(r) {
                            p.insert(path);
                        }
                    }
                }
            }
            return;
        }
        let Set::Rows(ranges) = &mut self.set else {
            return;
        };
        match mode {
            Mode::Replace => {
                *ranges = vec![(row, row)];
                self.anchor = Some(row);
            }
            Mode::Toggle => {
                let selected = {
                    let i = ranges.partition_point(|&(_, b)| b < row);
                    ranges.get(i).is_some_and(|&(a, _)| a <= row)
                };
                if selected {
                    remove_row(ranges, row);
                } else {
                    ranges.push((row, row));
                    normalize(ranges);
                }
                self.anchor = Some(row);
            }
            Mode::Extend => {
                let anchor = self.anchor.unwrap_or(row);
                *ranges = vec![(anchor.min(row), anchor.max(row))];
            }
            Mode::AddRange => {
                let anchor = self.anchor.unwrap_or(row);
                ranges.push((anchor.min(row), anchor.max(row)));
                normalize(ranges);
            }
        }
    }

    /// The selected rows below `limit` rows in total, in order (for actions).
    #[must_use]
    pub fn rows(&self, limit: u64) -> Vec<u64> {
        match &self.set {
            Set::Rows(r) => r
                .iter()
                .flat_map(|&(a, b)| a..=b)
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .collect(),
            Set::Paths(_) => Vec::new(),
        }
    }

    /// Pinned paths, if the selection is pinned.
    #[must_use]
    pub fn paths(&self) -> Option<&HashSet<Vec<u8>>> {
        match &self.set {
            Set::Paths(p) => Some(p),
            Set::Rows(_) => None,
        }
    }

    /// Before a live change: pin a small selection to its files, if all of them are known.
    pub fn pin(&mut self, path_of: &dyn Fn(u64) -> Option<Vec<u8>>) {
        let Set::Rows(ranges) = &self.set else {
            return;
        };
        if self.count() > PIN_LIMIT || ranges.is_empty() {
            return;
        }
        let mut paths = HashSet::new();
        for (a, b) in ranges.clone() {
            for r in a..=b {
                match path_of(r) {
                    Some(p) => {
                        paths.insert(p);
                    }
                    None => return,
                }
            }
        }
        self.set = Set::Paths(paths);
        self.anchor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(r: u64) -> Option<Vec<u8>> {
        Some(format!("/f{r}").into_bytes())
    }

    #[test]
    fn clicks() {
        let mut s = Selection::default();
        s.click(5, Mode::Replace, &path);
        assert!(s.contains(5, None) && !s.contains(4, None));
        s.click(8, Mode::Extend, &path);
        assert_eq!(s.rows(100), vec![5, 6, 7, 8]);
        s.click(6, Mode::Toggle, &path);
        assert_eq!(s.rows(100), vec![5, 7, 8]);
        s.click(10, Mode::AddRange, &path);
        assert_eq!(
            s.rows(100),
            vec![5, 6, 7, 8, 9, 10],
            "range from the anchor (6)"
        );
        assert_eq!(s.count(), 6);
        s.select_all(5_000_000);
        assert_eq!(s.count(), 5_000_000);
        assert!(s.contains(4_999_999, None));
        assert_eq!(s.rows(3), vec![0, 1, 2]);
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn pinned_selections_follow_files() {
        let mut s = Selection::default();
        s.click(2, Mode::Replace, &path);
        s.click(3, Mode::Extend, &path);
        s.pin(&path);
        assert_eq!(s.count(), 2);
        // After the change the files are at other rows.
        assert!(s.contains(9, Some(b"/f2")));
        assert!(!s.contains(2, Some(b"/fX")));
        s.click(4, Mode::Toggle, &path);
        assert_eq!(s.count(), 3);
        s.click(1, Mode::Replace, &path);
        assert_eq!(s.rows(10), vec![1], "a plain click starts over with rows");
        // Selections with unknown rows, or too big ones, stay row based.
        let mut big = Selection::default();
        big.select_all(PIN_LIMIT + 1);
        big.pin(&path);
        assert!(big.paths().is_none());
        let mut unknown = Selection::default();
        unknown.click(3, Mode::Replace, &path);
        unknown.pin(&|_| None);
        assert!(unknown.paths().is_none());
    }
}
