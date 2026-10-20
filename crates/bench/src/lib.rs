//! Synthetic file-tree generator for Fossick benchmarks and tests.
//!
//! Generates deterministic (seeded) trees with a realistic mix of names: common words, numbered
//! names, extensions, ~10% non-ASCII names and a small share of names that are not valid UTF-8.
//! Trees can be fed into an in-memory index or written to disk.

use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use fossick_core::{EntryId, EntryMeta, IndexConfig, IndexWriter};

/// Small, fast, deterministic PRNG (SplitMix64).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (n > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    /// True with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        #[allow(clippy::cast_precision_loss)]
        let x = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        x < p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        #[allow(clippy::cast_possible_truncation)]
        &items[self.below(items.len() as u64) as usize]
    }
}

const WORDS: &[&str] = &[
    "report", "image", "photo", "src", "main", "test", "config", "data", "backup", "notes",
    "index", "readme", "build", "lib", "util", "module", "player", "album", "track", "video",
    "invoice", "budget", "draft", "final", "project", "client", "server", "cache", "log",
    "settings", "theme", "icon", "font", "music", "movie", "scan", "doc", "manual", "guide",
    "resume", "letter", "archive", "export", "import", "sample", "demo", "temp", "old", "new",
    "home", "work", "school", "family", "holiday", "summer", "winter", "spring", "autumn", "game",
    "level", "shader", "texture", "model", "script", "plugin", "driver", "kernel", "package",
];

const NON_ASCII_WORDS: &[&str] = &[
    "café",
    "naïve",
    "Größe",
    "日本語",
    "данные",
    "résumé",
    "señor",
    "Ελληνικά",
    "Übersicht",
    "façade",
    "smörgåsbord",
    "Łódź",
    "Ærø",
    "中文文档",
    "مستند",
    "fiancée",
    "Straße",
    "crème",
];

const EXTENSIONS: &[&str] = &[
    "txt", "md", "pdf", "jpg", "png", "mp3", "flac", "mp4", "mkv", "rs", "c", "h", "py", "js",
    "json", "toml", "html", "css", "zip", "gz", "tar", "so", "o", "log", "docx", "xlsx", "svg",
    "webp", "iso", "deb",
];

/// Tree shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Mixed fan-out like a real home directory or source tree.
    Realistic,
    /// Long directory chains (depth 40+).
    Deep,
    /// Few directories with tens of thousands of entries each.
    Wide,
}

/// What to generate.
#[derive(Clone, Copy, Debug)]
pub struct TreeSpec {
    pub entries: usize,
    pub seed: u64,
    pub shape: Shape,
    /// Fraction of names containing non-ASCII characters.
    pub non_ascii: f64,
    /// Fraction of names containing a byte that is not valid UTF-8.
    pub invalid_utf8: f64,
}

impl TreeSpec {
    #[must_use]
    pub fn new(entries: usize) -> Self {
        Self {
            entries,
            seed: 42,
            shape: Shape::Realistic,
            non_ascii: 0.10,
            invalid_utf8: 0.002,
        }
    }
}

/// Receives the generated tree. Directory handles are indices assigned by the sink.
pub trait TreeSink {
    /// Creates a directory and returns its handle.
    fn dir(&mut self, parent: usize, name: &[u8], mtime: i64) -> usize;
    fn file(&mut self, parent: usize, name: &[u8], size: u64, mtime: i64);
}

/// Generates a random file name.
pub fn random_name(rng: &mut Rng, spec: &TreeSpec, is_dir: bool) -> Vec<u8> {
    let mut name = Vec::with_capacity(24);
    if rng.chance(spec.non_ascii) {
        name.extend_from_slice(rng.pick(NON_ASCII_WORDS).as_bytes());
    } else {
        let w = rng.pick(WORDS);
        if rng.chance(0.3) {
            let mut c = w.chars();
            if let Some(first) = c.next() {
                name.extend(first.to_uppercase().to_string().bytes());
                name.extend_from_slice(c.as_str().as_bytes());
            }
        } else {
            name.extend_from_slice(w.as_bytes());
        }
    }
    if rng.chance(0.5) {
        name.push(*rng.pick(b"_- ."));
        name.extend_from_slice(rng.pick(WORDS).as_bytes());
    }
    if rng.chance(0.6) {
        name.extend_from_slice(rng.below(10_000).to_string().as_bytes());
    }
    if rng.chance(spec.invalid_utf8) {
        name.push(0xFF);
    }
    if !is_dir && rng.chance(0.9) {
        name.push(b'.');
        name.extend_from_slice(rng.pick(EXTENSIONS).as_bytes());
    }
    name
}

fn name_hash(name: &[u8]) -> u64 {
    use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
    BuildHasherDefault::<DefaultHasher>::default().hash_one(name)
}

fn random_size(rng: &mut Rng) -> u64 {
    // Log-uniform between 0 and ~4 GiB, with many small files.
    if rng.chance(0.03) {
        return 0;
    }
    let bits = rng.below(32);
    rng.below(1u64 << bits) + 1
}

/// Generates a tree into `sink`, starting at directory handle `root`. Returns entries created.
pub fn generate(spec: &TreeSpec, sink: &mut impl TreeSink, root: usize) -> usize {
    let mut rng = Rng::new(spec.seed);
    let base_time: i64 = 1_600_000_000;
    let mut created = 0usize;
    let mut queue = std::collections::VecDeque::from([(root, 0usize)]);
    let mut used = std::collections::HashSet::new();
    while created < spec.entries {
        let Some((dir, depth)) = queue.pop_front() else {
            // Ran out of directories: keep filling the root.
            queue.push_back((root, 0));
            continue;
        };
        let (n_children, dir_share) = match spec.shape {
            Shape::Realistic => (
                1 + rng.below(if depth < 2 { 40 } else { 25 }) as usize,
                0.12,
            ),
            Shape::Deep => (
                2 + rng.below(4) as usize,
                if depth < 64 { 0.5 } else { 0.0 },
            ),
            Shape::Wide => (5_000 + rng.below(30_000) as usize, 0.002),
        };
        let mut made_dir = false;
        for _ in 0..n_children {
            if created >= spec.entries {
                break;
            }
            let is_dir = rng.chance(dir_share) || (!made_dir && depth < 3);
            let mut name = random_name(&mut rng, spec, is_dir);
            // Names must be unique within a directory. Track (dir, hash) pairs so the generator
            // does not make one heap allocation per name (that would fragment the heap and
            // distort memory measurements); a hash collision just adds a suffix.
            let mut attempt = 0;
            while !used.insert((dir, name_hash(&name))) {
                attempt += 1;
                name.extend_from_slice(format!("~{attempt}").as_bytes());
            }
            #[allow(clippy::cast_possible_wrap)]
            let mtime = base_time + rng.below(150_000_000) as i64;
            if is_dir {
                made_dir = true;
                let h = sink.dir(dir, &name, mtime);
                queue.push_back((h, depth + 1));
            } else {
                sink.file(dir, &name, random_size(&mut rng), mtime);
            }
            created += 1;
        }
        if !made_dir && queue.is_empty() {
            queue.push_back((dir, depth));
        }
    }
    created
}

/// Sink that inserts into an in-memory index.
#[derive(Debug)]
pub struct IndexSink<'a> {
    pub writer: &'a mut IndexWriter,
    pub dirs: Vec<EntryId>,
}

impl<'a> IndexSink<'a> {
    pub fn new(writer: &'a mut IndexWriter, root: EntryId) -> Self {
        Self {
            writer,
            dirs: vec![root],
        }
    }
}

impl TreeSink for IndexSink<'_> {
    fn dir(&mut self, parent: usize, name: &[u8], mtime: i64) -> usize {
        let id = self
            .writer
            .insert(self.dirs[parent], name, &EntryMeta::dir(mtime))
            .expect("generator produces unique valid names");
        self.dirs.push(id);
        self.dirs.len() - 1
    }

    fn file(&mut self, parent: usize, name: &[u8], size: u64, mtime: i64) {
        self.writer
            .insert(self.dirs[parent], name, &EntryMeta::file(size, mtime))
            .expect("generator produces unique valid names");
    }
}

/// Builds and publishes an in-memory index with one volume mounted at `/bench`.
#[must_use]
pub fn build_index(spec: &TreeSpec) -> (IndexWriter, EntryId) {
    let mut w = IndexWriter::new(IndexConfig::default());
    let (_, root) = w
        .add_volume(
            "bench",
            b"/bench",
            "ext4",
            None,
            &EntryMeta::dir(1_600_000_000),
        )
        .expect("first volume");
    {
        let mut sink = IndexSink::new(&mut w, root);
        generate(spec, &mut sink, 0);
    }
    w.publish();
    (w, root)
}

/// Sink that creates the tree on disk (empty files; sizes are not materialized).
#[derive(Debug)]
pub struct DiskSink {
    pub dirs: Vec<PathBuf>,
    pub errors: usize,
}

impl DiskSink {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            dirs: vec![root.to_path_buf()],
            errors: 0,
        }
    }
}

impl TreeSink for DiskSink {
    fn dir(&mut self, parent: usize, name: &[u8], _mtime: i64) -> usize {
        let p = self.dirs[parent].join(std::ffi::OsStr::from_bytes(name));
        if fs::create_dir(&p).is_err() {
            self.errors += 1;
        }
        self.dirs.push(p);
        self.dirs.len() - 1
    }

    fn file(&mut self, parent: usize, name: &[u8], _size: u64, _mtime: i64) {
        let p = self.dirs[parent].join(std::ffi::OsStr::from_bytes(name));
        if fs::File::create(&p).is_err() {
            self.errors += 1;
        }
    }
}

/// Creates the tree under `root` on disk.
pub fn generate_on_disk(spec: &TreeSpec, root: &Path) -> io::Result<usize> {
    fs::create_dir_all(root)?;
    let mut sink = DiskSink::new(root);
    let n = generate(spec, &mut sink, 0);
    if sink.errors > 0 {
        return Err(io::Error::other(format!("{} entries failed", sink.errors)));
    }
    Ok(n)
}

/// Current and peak resident set size in bytes (from `/proc/self/status`).
#[must_use]
pub fn rss_bytes() -> (u64, u64) {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |key: &str| {
        status
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(0, |kb| kb * 1024)
    };
    (field("VmRSS:"), field("VmHWM:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_requested_count_deterministically() {
        for shape in [Shape::Realistic, Shape::Deep, Shape::Wide] {
            let mut spec = TreeSpec::new(20_000);
            spec.shape = shape;
            let (w1, _) = build_index(&spec);
            let (w2, _) = build_index(&spec);
            assert_eq!(w1.len(), 20_001);
            let g1 = w1.handle().load();
            let g2 = w2.handle().load();
            let n1: Vec<Vec<u8>> = g1
                .orders()
                .path
                .iter()
                .map(|i| g1.path(EntryId(i)))
                .collect();
            let n2: Vec<Vec<u8>> = g2
                .orders()
                .path
                .iter()
                .map(|i| g2.path(EntryId(i)))
                .collect();
            assert_eq!(n1, n2);
            w1.check_invariants().unwrap();
        }
    }

    #[test]
    fn writes_tree_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let n = generate_on_disk(&TreeSpec::new(500), &tmp.path().join("t")).unwrap();
        assert_eq!(n, 500);
    }
}
