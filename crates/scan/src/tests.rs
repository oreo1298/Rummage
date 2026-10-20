use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use fossick_core::{EntryMeta, FileKind, IndexConfig, IndexWriter};

use super::*;

fn build(root: &Path) -> (IndexWriter, ScanStats) {
    let mut w = IndexWriter::new(IndexConfig::default());
    let (meta, _, _) = stat_path(root).unwrap();
    let (_, r) = w
        .add_volume("test", root.as_os_str().as_bytes(), "test", None, &meta)
        .unwrap();
    let stats = scan_into(root, &mut w, r, &ScanOptions::default()).unwrap();
    w.publish();
    (w, stats)
}

#[test]
fn scans_tree_with_symlinks_and_odd_names() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("a/b/c")).unwrap();
    fs::write(root.join("a/one.txt"), b"hello").unwrap();
    fs::write(root.join("a/b/two.bin"), vec![0u8; 5000]).unwrap();
    fs::write(root.join("a/b/c/three"), b"x").unwrap();
    symlink("a", root.join("link-to-a")).unwrap();
    symlink("/nonexistent", root.join("dangling")).unwrap();
    fs::write(root.join(OsStr::from_bytes(b"bad\xFFname")), b"").unwrap();
    fs::write(root.join("Ünïcödé.md"), b"").unwrap();
    let (w, stats) = build(root);
    assert_eq!(stats.errors, 0, "{:?}", stats.error_samples);
    let g = w.handle().load();
    let mut paths: Vec<String> = g
        .iter()
        .map(|id| {
            let p = g.path(id);
            String::from_utf8_lossy(&p[root.as_os_str().len()..]).into_owned()
        })
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            "",
            "/a",
            "/a/b",
            "/a/b/c",
            "/a/b/c/three",
            "/a/b/two.bin",
            "/a/one.txt",
            "/bad\u{FFFD}name",
            "/dangling",
            "/link-to-a",
            "/Ünïcödé.md",
        ]
    );
    let root_id = g.volumes()[0].root;
    // Files: 5 + 5000 + 1; symlinks count their target length: "a" (1) + "/nonexistent" (12).
    assert_eq!(g.size(root_id), 5 + 5000 + 1 + 1 + 12);
    let link = w.lookup(root_id, b"link-to-a").unwrap();
    assert_eq!(g.kind(link), FileKind::Symlink);
    assert_eq!(g.size(link), 1, "symlink size is the target length");
    assert!(w.lookup(root_id, b"bad\xFFname").is_some());
    w.check_invariants().unwrap();
}

#[test]
fn unreadable_directories_are_reported_not_fatal() {
    if rustix::process::geteuid().is_root() {
        // Root bypasses permission checks; covered by the multi-user tests in Phase 2.
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("locked")).unwrap();
    fs::write(root.join("locked/f"), b"").unwrap();
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
    let (w, stats) = build(root);
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(stats.errors, 1);
    assert_eq!(w.len(), 2);
}

#[test]
fn large_flat_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for i in 0..3000 {
        fs::write(root.join(format!("f{i}")), b"").unwrap();
    }
    let (w, stats) = build(root);
    assert_eq!(stats.entries, 3000);
    assert_eq!(w.len(), 3001);
    let _ = EntryMeta::default();
}

#[test]
fn content_search_reads_files() {
    use std::sync::atomic::AtomicBool;

    use fossick_core::query::{
        Clock, CompileContext, Macros, NumericIds, SearchOptions, compile_str,
    };
    use fossick_core::search::{SearchContext, Sort, search};

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::write(root.join("a.txt"), b"hello world").unwrap();
    fs::write(root.join("b.txt"), b"HELLO there").unwrap();
    // Needle straddling the 256 KiB read boundary.
    let mut big = vec![b'x'; 256 * 1024 - 3];
    big.extend_from_slice(b"needle");
    fs::write(root.join("big.bin"), &big).unwrap();
    fs::write(root.join("c.txt"), b"nothing").unwrap();
    let (w, _) = build(root);
    let g = w.handle().load();
    let macros = Macros::default();
    let ctx = CompileContext {
        options: SearchOptions::default(),
        clock: Clock::system(),
        macros: &macros,
        users: &NumericIds,
    };
    let names = |q: &str| {
        let c = compile_str(q, &ctx).unwrap();
        let cancel = AtomicBool::new(false);
        let mut sc = SearchContext::basic(&cancel);
        sc.content = Some(&content::FsContent);
        let r = search(&g, &c, Sort::default(), &sc).unwrap();
        r.ids
            .iter()
            .map(|&id| String::from_utf8_lossy(g.name(id)).into_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(names("content:hello"), ["a.txt", "b.txt"]);
    assert_eq!(names("case:content:hello"), ["a.txt"]);
    assert_eq!(names("content:needle"), ["big.bin"]);
    assert_eq!(names("ext:txt !content:hello"), ["c.txt"]);
    assert_eq!(names("regex:content:\"wor.d\""), ["a.txt"]);
}
