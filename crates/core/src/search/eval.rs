//! Page-vectorized predicate evaluation.
//!
//! Every predicate is evaluated for one 4096-slot page at a time, producing a 64-word bitmap.
//! AND passes its running candidate mask to later children so expensive leaves only look at
//! entries that are still candidates; OR only evaluates entries not matched yet.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::sync::atomic::Ordering as AtomicOrdering;

use rayon::prelude::*;
use smallvec::SmallVec;

use super::{SearchContext, content_matches};
use crate::index::names::SideKind;
use crate::index::{EntryId, Generation, IndexData, PAGE_SIZE, TimeField, flags};
use crate::meta::{Attrs, FileKind};
use crate::name;
use crate::query::KindOnly;
use crate::query::compile::{AttribSet, ChildKind, DupeProp, PermTest, Pred};
use crate::query::matcher::{TextMatch, TextMode, normalize_into};
use crate::util::Bitmap;

pub(crate) const WORDS: usize = PAGE_SIZE / 64;
pub(crate) type Bits = [u64; WORDS];

#[inline]
fn popcount(b: &Bits) -> u32 {
    b.iter().map(|w| w.count_ones()).sum()
}

#[inline]
fn is_zero(b: &Bits) -> bool {
    b.iter().all(|&w| w == 0)
}

/// Iterates the set bits of a page bitmap as slot numbers.
fn slots(b: &Bits) -> impl Iterator<Item = usize> + '_ {
    b.iter().enumerate().flat_map(|(wi, &w)| {
        let mut w = w;
        std::iter::from_fn(move || {
            if w == 0 {
                return None;
            }
            let t = w.trailing_zeros() as usize;
            w &= w - 1;
            Some(wi * 64 + t)
        })
    })
}

#[inline]
fn set(b: &mut Bits, slot: usize) {
    b[slot / 64] |= 1 << (slot % 64);
}

#[inline]
fn test(b: &Bits, slot: usize) -> bool {
    b[slot / 64] & (1 << (slot % 64)) != 0
}

/// Per-directory memo for substring path matching: whether the directory’s path (with a
/// trailing `/`) contains the needle, and the last `needle.len() - 1` bytes of it.
#[derive(Debug)]
struct PathState {
    contains: bool,
    tail: SmallVec<[u8; 32]>,
}

/// Per-query state for one `Path` predicate.
struct PathMemo {
    dirs: Vec<OnceLock<PathState>>,
    /// Full normalized directory paths (regex / wildcard matching only).
    full: Vec<OnceLock<Box<[u8]>>>,
}

/// Evaluates predicates against one generation.
pub(crate) struct Engine<'a> {
    pub(crate) g: &'a Generation,
    d: &'a IndexData,
    ctx: &'a SearchContext<'a>,
    /// Precomputed bitmaps for global predicates (`child:`, `dupe:`), keyed by node address.
    globals: HashMap<usize, Bitmap>,
    /// Parent-path predicates: matching directory entries, keyed by node address.
    parents: HashMap<usize, HashSet<u32>>,
    paths: HashMap<usize, PathMemo>,
}

fn key(p: &Pred) -> usize {
    std::ptr::from_ref(p) as usize
}

impl<'a> Engine<'a> {
    pub(crate) fn new(g: &'a Generation, pred: &Pred, ctx: &'a SearchContext<'a>) -> Self {
        let mut e = Self {
            g,
            d: &g.data,
            ctx,
            globals: HashMap::new(),
            parents: HashMap::new(),
            paths: HashMap::new(),
        };
        e.prepare(pred);
        e
    }

    /// Precomputes global predicate results (children first, so nested ones work).
    fn prepare(&mut self, p: &Pred) {
        match p {
            Pred::And(c) | Pred::Or(c) => {
                for x in c {
                    self.prepare(x);
                }
            }
            Pred::Not(x) => self.prepare(x),
            Pred::Child(kind, inner) => {
                self.prepare(inner);
                let hits = self.evaluate_all(inner);
                let mut parents = Bitmap::with_capacity(self.d.capacity());
                for (wi, &w) in hits.words().iter().enumerate() {
                    let mut w = w;
                    while w != 0 {
                        let id = u32::try_from(wi * 64 + w.trailing_zeros() as usize)
                            .unwrap_or(u32::MAX);
                        w &= w - 1;
                        let is_dir = self.d.is_dir(id);
                        let ok = match kind {
                            ChildKind::Any => true,
                            ChildKind::Files => !is_dir,
                            ChildKind::Folders => is_dir,
                        };
                        if ok && let Some(parent) = self.d.parent_of(id) {
                            parents.set(parent);
                        }
                    }
                }
                self.globals.insert(key(p), parents);
            }
            Pred::Dupe(props) => {
                let b = self.dupes(props);
                self.globals.insert(key(p), b);
            }
            Pred::Parent { path, case } => {
                let set = self.dirs_with_path(path, *case);
                self.parents.insert(key(p), set);
            }
            Pred::Path(_) => {
                let n = self.d.dirs.capacity();
                self.paths.insert(
                    key(p),
                    PathMemo {
                        dirs: (0..n).map(|_| OnceLock::new()).collect(),
                        full: (0..n).map(|_| OnceLock::new()).collect(),
                    },
                );
            }
            _ => {}
        }
    }

    /// Evaluates `p` over the whole index (respecting visibility) into one bitmap.
    pub(crate) fn evaluate_all(&self, p: &Pred) -> Bitmap {
        let pages = self.d.page_count();
        let words: Vec<Bits> = (0..pages)
            .into_par_iter()
            .map(|page| {
                if self.ctx.cancel.load(AtomicOrdering::Relaxed) {
                    return [0; WORDS];
                }
                let cand = self.base_mask(page);
                if is_zero(&cand) {
                    return cand;
                }
                self.eval(p, page, &cand)
            })
            .collect();
        let mut b = Bitmap::with_capacity(pages * PAGE_SIZE);
        for (page, w) in words.iter().enumerate() {
            b.words_mut()[page * WORDS..(page + 1) * WORDS].copy_from_slice(w);
        }
        b
    }

    /// Live (and visible) entries of a page.
    fn base_mask(&self, page: usize) -> Bits {
        let mut m = [0u64; WORDS];
        let fl = self.d.flags.page(page);
        for (slot, &f) in fl.iter().enumerate() {
            if f & flags::FREE == 0 {
                set(&mut m, slot);
            }
        }
        if let Some(vis) = self.ctx.visible {
            let words = vis.words();
            for (i, w) in m.iter_mut().enumerate() {
                *w &= words.get(page * WORDS + i).copied().unwrap_or(0);
            }
        }
        m
    }

    #[inline]
    fn id(page: usize, slot: usize) -> u32 {
        u32::try_from(page * PAGE_SIZE + slot).unwrap_or(u32::MAX)
    }

    /// Evaluates `p` for the candidates `cand` of `page`.
    pub(crate) fn eval(&self, p: &Pred, page: usize, cand: &Bits) -> Bits {
        match p {
            Pred::True => *cand,
            Pred::False => [0; WORDS],
            Pred::And(children) => {
                let mut cur = *cand;
                for c in children {
                    if is_zero(&cur) {
                        break;
                    }
                    cur = self.eval(c, page, &cur);
                }
                cur
            }
            Pred::Or(children) => {
                let mut acc = [0u64; WORDS];
                let mut rest = *cand;
                for c in children {
                    if is_zero(&rest) {
                        break;
                    }
                    let r = self.eval(c, page, &rest);
                    for i in 0..WORDS {
                        acc[i] |= r[i];
                        rest[i] &= !r[i];
                    }
                }
                acc
            }
            Pred::Not(c) => {
                let r = self.eval(c, page, cand);
                let mut out = *cand;
                for i in 0..WORDS {
                    out[i] &= !r[i];
                }
                out
            }
            Pred::Name(m) => self.eval_name(m, page, cand),
            Pred::Path(m) => {
                let memo = self.paths.get(&key(p));
                self.filter(page, cand, |id| {
                    memo.is_some_and(|memo| self.path_match(m, memo, id))
                })
            }
            Pred::Child(..) | Pred::Dupe(_) => {
                let b = self.globals.get(&key(p));
                self.filter(page, cand, |id| b.is_some_and(|b| b.get(id)))
            }
            Pred::Parent { .. } => {
                let set = self.parents.get(&key(p));
                self.filter(page, cand, |id| {
                    set.is_some_and(|s| self.d.parent_of(id).is_some_and(|par| s.contains(&par)))
                })
            }
            Pred::Content(m) => self.filter(page, cand, |id| {
                !self.ctx.cancel.load(AtomicOrdering::Relaxed)
                    && content_matches(self.g, self.ctx, m, EntryId(id))
            }),
            leaf => self.filter(page, cand, |id| self.leaf(leaf, id)),
        }
    }

    fn filter(&self, page: usize, cand: &Bits, f: impl Fn(u32) -> bool) -> Bits {
        let mut out = [0u64; WORDS];
        for slot in slots(cand) {
            if f(Self::id(page, slot)) {
                set(&mut out, slot);
            }
        }
        out
    }

    /// Name matching for one page: raw-arena scan for ASCII names, side arena for the others.
    fn eval_name(&self, m: &TextMatch, page: usize, cand: &Bits) -> Bits {
        let mut out = [0u64; WORDS];
        let Some(names) = self.d.names.get(page) else {
            return out;
        };
        let fl = self.d.flags.page(page);
        let non_ascii = |slot: usize| fl.get(slot).is_some_and(|f| f & flags::NON_ASCII != 0);
        if m.ascii_only {
            for slot in slots(cand) {
                if m.is_match(names.name(slot)) {
                    set(&mut out, slot);
                }
            }
            return out;
        }
        // ASCII names.
        let arena_scan =
            matches!(m.mode, TextMode::Substring) && m.needle_ascii && popcount(cand) >= 256;
        if arena_scan {
            let arena = names.arena();
            let offs = names.offsets();
            let mut pos = 0;
            while let Some(hit) = m.find_raw(arena, pos) {
                // The slot whose name contains `hit` (NUL separators mean no cross-name hits).
                let slot = offs
                    .partition_point(|&o| (o as usize) <= hit)
                    .saturating_sub(1);
                if slot < PAGE_SIZE && test(cand, slot) && !non_ascii(slot) {
                    set(&mut out, slot);
                }
                pos = offs.get(slot + 1).map_or(arena.len(), |&o| o as usize);
            }
        } else {
            for slot in slots(cand) {
                if !non_ascii(slot) && m.is_match(names.name(slot)) {
                    set(&mut out, slot);
                }
            }
        }
        // Non-ASCII names, through the normalized side arena.
        let any_non_ascii = slots(cand).any(non_ascii);
        if any_non_ascii {
            let kind = SideKind {
                fold_case: m.folds_case(),
                strip_diacritics: !m.diacritics,
            };
            let side = names.side(kind, fl);
            for i in 0..side.len() {
                let slot = side.slots[i] as usize;
                if test(cand, slot) && m.is_match_normalized(side.name(i)) {
                    set(&mut out, slot);
                }
            }
        }
        out
    }

    /// Normalized name of an entry for matcher `m`.
    fn norm_name(m: &TextMatch, name: &[u8], out: &mut Vec<u8>) {
        normalize_into(name, !m.folds_case(), m.diacritics, m.ascii_only, out);
    }

    fn dir_state<'m>(&self, m: &TextMatch, memo: &'m PathMemo, dir_entry: u32) -> &'m PathState {
        let dir = self.d.dir_of(dir_entry).unwrap_or(0) as usize;
        if let Some(s) = memo.dirs.get(dir).and_then(OnceLock::get) {
            return s;
        }
        // Collect the uncomputed ancestors, then fill them top-down (no recursion).
        let mut chain = vec![dir_entry];
        let mut cur = dir_entry;
        while let Some(p) = self.d.parent_of(cur) {
            let pd = self.d.dir_of(p).unwrap_or(0) as usize;
            if memo.dirs.get(pd).and_then(OnceLock::get).is_some()
                || chain.len() > crate::index::MAX_DEPTH
            {
                break;
            }
            chain.push(p);
            cur = p;
        }
        let keep = m.needle.len().saturating_sub(1);
        for &e in chain.iter().rev() {
            let d = self.d.dir_of(e).unwrap_or(0) as usize;
            let Some(cell) = memo.dirs.get(d) else {
                continue;
            };
            cell.get_or_init(|| {
                let mut buf: Vec<u8> = Vec::with_capacity(64);
                let mut inherited = false;
                match self.d.parent_of(e) {
                    Some(p) => {
                        let pd = self.d.dir_of(p).unwrap_or(0) as usize;
                        if let Some(ps) = memo.dirs.get(pd).and_then(OnceLock::get) {
                            inherited = ps.contains;
                            buf.extend_from_slice(&ps.tail);
                        }
                        Self::norm_name(m, self.d.name_of(e), &mut buf);
                    }
                    None => {
                        let mount = self.d.volume_of(e).map_or(&b"/"[..], |v| &v.mount_path[..]);
                        Self::norm_name(m, mount, &mut buf);
                        if buf.last() == Some(&b'/') {
                            buf.pop();
                        }
                    }
                }
                buf.push(b'/');
                let contains = inherited || m.is_match_normalized(&buf);
                let tail = SmallVec::from_slice(&buf[buf.len().saturating_sub(keep)..]);
                PathState { contains, tail }
            });
        }
        memo.dirs
            .get(dir)
            .and_then(OnceLock::get)
            .unwrap_or_else(|| {
                static EMPTY: OnceLock<PathState> = OnceLock::new();
                EMPTY.get_or_init(|| PathState {
                    contains: false,
                    tail: SmallVec::new(),
                })
            })
    }

    fn full_dir_path<'m>(&self, m: &TextMatch, memo: &'m PathMemo, dir_entry: u32) -> &'m [u8] {
        let dir = self.d.dir_of(dir_entry).unwrap_or(0) as usize;
        let Some(cell) = memo.full.get(dir) else {
            return &[];
        };
        cell.get_or_init(|| {
            let raw = self.g.path(EntryId(dir_entry));
            let mut out = Vec::with_capacity(raw.len());
            Self::norm_name(m, &raw, &mut out);
            out.into_boxed_slice()
        })
    }

    fn path_match(&self, m: &TextMatch, memo: &PathMemo, id: u32) -> bool {
        let Some(parent) = self.d.parent_of(id) else {
            // A volume root: its path is the mount path.
            let mount = self
                .d
                .volume_of(id)
                .map_or(&b"/"[..], |v| &v.mount_path[..]);
            return m.is_match(mount);
        };
        match &m.mode {
            TextMode::Substring => {
                let st = self.dir_state(m, memo, parent);
                if st.contains {
                    return true;
                }
                let mut buf: SmallVec<[u8; 128]> = SmallVec::new();
                buf.extend_from_slice(&st.tail);
                let mut name = Vec::new();
                Self::norm_name(m, self.d.name_of(id), &mut name);
                buf.extend_from_slice(&name);
                m.is_match_normalized(&buf)
            }
            _ => {
                let dir = self.full_dir_path(m, memo, parent);
                let mut buf = Vec::with_capacity(dir.len() + 32);
                buf.extend_from_slice(dir);
                if buf.last() != Some(&b'/') {
                    buf.push(b'/');
                }
                Self::norm_name(m, self.d.name_of(id), &mut buf);
                m.is_match_normalized(&buf)
            }
        }
    }

    fn dirs_with_path(&self, path: &[u8], case: bool) -> HashSet<u32> {
        let last = path
            .rsplit(|&b| b == b'/')
            .find(|c| !c.is_empty())
            .unwrap_or(b"");
        let mut out = HashSet::new();
        let norm = |b: &[u8]| {
            let mut v = Vec::new();
            normalize_into(b, case, true, false, &mut v);
            v
        };
        let cands: Vec<u32> = (0..u32::try_from(self.d.capacity()).unwrap_or(0))
            .into_par_iter()
            .filter(|&id| {
                self.d.is_live(id) && self.d.is_dir(id) && {
                    let n = self.d.name_of(id);
                    if last.is_empty() {
                        self.d.parent_of(id).is_none()
                    } else {
                        norm(n) == last
                    }
                }
            })
            .collect();
        for id in cands {
            let p = norm(&self.g.path(EntryId(id)));
            let p = if p.len() > 1 && p.last() == Some(&b'/') {
                &p[..p.len() - 1]
            } else {
                &p[..]
            };
            if p == path {
                out.insert(id);
            }
        }
        out
    }

    fn dupes(&self, props: &[DupeProp]) -> Bitmap {
        let d = self.d;
        let all = self.evaluate_all(&Pred::True);
        let ids: Vec<u32> = (0..u32::try_from(d.capacity()).unwrap_or(0))
            .filter(|&id| all.get(id))
            .collect();
        let dupe_key = |id: u32| -> Vec<u8> {
            let mut k = Vec::new();
            for p in props {
                let name = d.name_of(id);
                match p {
                    DupeProp::Name => crate::fold::collation_key_into(name, &mut k),
                    DupeProp::NamePart => crate::fold::collation_key_into(name::stem(name), &mut k),
                    DupeProp::Ext => crate::fold::collation_key_into(
                        name::extension(name).unwrap_or(b""),
                        &mut k,
                    ),
                    DupeProp::Size => k.extend_from_slice(&d.size_of(id).to_le_bytes()),
                    DupeProp::Modified => k.extend_from_slice(
                        &d.time_secs(id, TimeField::Modified)
                            .unwrap_or(i64::MIN)
                            .to_le_bytes(),
                    ),
                    DupeProp::Created => k.extend_from_slice(
                        &d.time_secs(id, TimeField::Created)
                            .unwrap_or(i64::MIN)
                            .to_le_bytes(),
                    ),
                }
                k.push(0xFF);
            }
            k
        };
        let mut counts: HashMap<Vec<u8>, u32> = HashMap::new();
        for &id in &ids {
            *counts.entry(dupe_key(id)).or_insert(0) += 1;
        }
        let mut b = Bitmap::with_capacity(d.capacity());
        for &id in &ids {
            if counts.get(&dupe_key(id)).copied().unwrap_or(0) > 1 {
                b.set(id);
            }
        }
        b
    }

    fn leaf(&self, p: &Pred, id: u32) -> bool {
        let d = self.d;
        match p {
            Pred::Kind(KindOnly::Files) => !d.is_dir(id),
            Pred::Kind(KindOnly::Folders) => d.is_dir(id),
            Pred::Kind(KindOnly::Any) => true,
            Pred::Ext { exts, case } => {
                if d.is_dir(id) {
                    return false;
                }
                let ext = name::extension(d.name_of(id)).unwrap_or(b"");
                let mut buf: SmallVec<[u8; 16]> = SmallVec::new();
                if *case || ext.is_ascii() {
                    if *case {
                        buf.extend_from_slice(ext);
                    } else {
                        buf.extend(ext.iter().map(u8::to_ascii_lowercase));
                    }
                    exts.contains(buf.as_slice())
                } else {
                    let mut v = Vec::new();
                    normalize_into(ext, false, true, false, &mut v);
                    exts.contains(&v)
                }
            }
            Pred::Size(r) => r.contains(i128::from(d.size_of(id))),
            Pred::Time(field, r) => d
                .time_secs(id, *field)
                .is_some_and(|t| r.contains(i128::from(t))),
            Pred::TimeUnknown(field) => d.time_secs(id, *field).is_none(),
            Pred::Len(r) => r.contains(crate::fold::char_len(d.name_of(id)) as i128),
            Pred::Depth(r) => r.contains(self.g.depth(EntryId(id)) as i128),
            Pred::ChildCount(kind, r) => {
                let Some(dir) = d.dir_of(id) else {
                    return false;
                };
                let (f, fo) = (
                    d.dirs.files.get(dir as usize),
                    d.dirs.folders.get(dir as usize),
                );
                let n = match kind {
                    ChildKind::Any => f + fo,
                    ChildKind::Files => f,
                    ChildKind::Folders => fo,
                };
                r.contains(i128::from(n))
            }
            Pred::Inode(r) => r.contains(i128::from(d.inode.get(id as usize))),
            Pred::VolumeIndex(r) => {
                let page = (id as usize) / PAGE_SIZE;
                r.contains(i128::from(d.page_volume.get(page).copied().unwrap_or(0)))
            }
            Pred::Uid(r) => r.contains(i128::from(d.owner_of(id).0)),
            Pred::Gid(r) => r.contains(i128::from(d.owner_of(id).1)),
            Pred::Empty => d.dir_of(id).is_some_and(|dir| {
                d.dirs.files.get(dir as usize) + d.dirs.folders.get(dir as usize) == 0
            }),
            Pred::Root => d.flags_of(id) & flags::VOLUME_ROOT != 0,
            Pred::Symlink => d.kind_of(id) == FileKind::Symlink,
            Pred::Hidden => d.flags_of(id) & flags::HIDDEN != 0,
            Pred::Attrib(a) => self.attrib(a, id),
            Pred::Perm(t) => {
                let bits = u32::from(d.mode_of(id)) & 0o7777;
                match *t {
                    PermTest::Exact(b) => bits == b,
                    PermTest::AllSet(b) => bits & b == b,
                    PermTest::AnySet(b) => bits & b != 0,
                    PermTest::NoneSet(b) => bits & b == 0,
                }
            }
            Pred::FsType(list) => d
                .volume_of(id)
                .is_some_and(|v| list.iter().any(|t| t.eq_ignore_ascii_case(&v.fstype))),
            Pred::Volume(m) => d.volume_of(id).is_some_and(|v| {
                m.is_match(&v.mount_path)
                    || m.is_match(v.key.as_bytes())
                    || v.label.as_deref().is_some_and(|l| m.is_match(l.as_bytes()))
            }),
            Pred::RunCount(r) => r.contains(i128::from(self.ctx.runs.run_count(EntryId(id)))),
            Pred::DateRun(r) => self
                .ctx
                .runs
                .last_run(EntryId(id))
                .is_some_and(|t| r.contains(i128::from(t))),
            Pred::Type(m) => {
                let t = super::type_name(self.g, self.ctx, EntryId(id));
                m.is_match(t.as_bytes())
            }
            // Handled in `eval`.
            _ => false,
        }
    }

    fn attrib(&self, a: &AttribSet, id: u32) -> bool {
        let d = self.d;
        let kind = d.kind_of(id);
        let mode = u32::from(d.mode_of(id));
        let attrs = Attrs(d.attrs.get(id as usize));
        let hidden = d.flags_of(id) & flags::HIDDEN != 0;
        let readonly = mode & 0o222 == 0;
        let offline = d.volume_of(id).is_some_and(|v| !v.online);
        (!a.never)
            && (!a.readonly || readonly)
            && (!a.hidden || hidden)
            && (!a.system || !matches!(kind, FileKind::File | FileKind::Dir | FileKind::Symlink))
            && (!a.directory || kind == FileKind::Dir)
            && (!a.archive || kind == FileKind::File)
            && (!a.normal || (kind == FileKind::File && !hidden && !readonly && attrs.bits() == 0))
            && (!a.sparse || attrs.contains(Attrs::SPARSE))
            && (!a.reparse || kind == FileKind::Symlink)
            && (!a.compressed || attrs.contains(Attrs::COMPRESSED))
            && (!a.offline || offline)
            && (!a.encrypted || attrs.contains(Attrs::ENCRYPTED))
            && (!a.integrity || attrs.contains(Attrs::VERITY))
    }
}
