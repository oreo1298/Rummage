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

/// Paths below `root` in the index, relative to it, sorted.
fn indexed(w: &IndexWriter, root: &Path) -> Vec<String> {
    let g = w.handle().load();
    let mut v: Vec<String> = g
        .iter()
        .map(|id| String::from_utf8_lossy(&g.path(id)[root.as_os_str().len()..]).into_owned())
        .collect();
    v.sort();
    v
}

/// The same set from a fresh walk of the filesystem.
fn on_disk(root: &Path) -> Vec<String> {
    let mut v = vec![String::new()];
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            v.push(
                String::from_utf8_lossy(&p.as_os_str().as_bytes()[root.as_os_str().len()..])
                    .into_owned(),
            );
            if e.file_type().unwrap().is_dir() {
                stack.push(p);
            }
        }
    }
    v.sort();
    v
}

fn rescan(w: &mut IndexWriter, root: &Path, prior: Option<&dyn Prior>) -> ScanStats {
    let root_entry = w.handle().load().volumes()[0].root;
    let mut applier = Applier::new(root_entry, true);
    let stats = scan_with_prior(root, &ScanOptions::default(), prior, |l| {
        applier.apply(w, l);
    })
    .unwrap();
    assert_eq!(applier.errors, 0);
    w.publish();
    w.check_invariants().unwrap();
    stats
}

fn make_tree(root: &Path) {
    for d in ["a/x", "a/y", "b/z/deep", "c"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    for f in ["a/x/1", "a/y/2", "b/z/deep/3", "c/4", "top"] {
        fs::write(root.join(f), f).unwrap();
    }
}

#[test]
fn full_rescan_converges_to_the_filesystem() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    make_tree(root);
    let (mut w, _) = build(root);
    assert_eq!(indexed(&w, root), on_disk(root));
    fs::remove_file(root.join("a/x/1")).unwrap();
    fs::remove_dir_all(root.join("b")).unwrap();
    fs::create_dir_all(root.join("new/dir")).unwrap();
    fs::write(root.join("new/dir/f"), b"").unwrap();
    fs::rename(root.join("c"), root.join("c2")).unwrap();
    fs::remove_file(root.join("top")).unwrap();
    fs::create_dir(root.join("top")).unwrap(); // a file became a directory
    rescan(&mut w, root, None);
    assert_eq!(indexed(&w, root), on_disk(root));
}

#[test]
fn pruned_rescan_lists_only_changed_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    make_tree(root);
    // Directories modified within the racy window of their listing are always read again;
    // let the tree age past it.
    std::thread::sleep(std::time::Duration::from_millis(120));
    let (mut w, stats) = build(root);
    let dirs = stats.dirs;
    std::thread::sleep(std::time::Duration::from_millis(60));
    // Change two directories: a/y (new file) and b/z/deep (deleted file).
    fs::write(root.join("a/y/new"), b"").unwrap();
    fs::remove_file(root.join("b/z/deep/3")).unwrap();
    let g = w.handle().load();
    let prior = IndexPrior::new(Arc::clone(&g), g.volumes()[0].root, i64::MAX);
    let stats = rescan(&mut w, root, Some(&prior));
    assert_eq!(stats.dirs, dirs);
    assert_eq!(stats.unchanged_dirs, dirs - 2);
    assert_eq!(indexed(&w, root), on_disk(root));
    // With a cutoff before everything, nothing counts as unchanged.
    let g = w.handle().load();
    let prior = IndexPrior::new(Arc::clone(&g), g.volumes()[0].root, 0);
    assert_eq!(rescan(&mut w, root, Some(&prior)).unchanged_dirs, 0);
}

#[test]
fn directory_acls_and_handles_are_captured() {
    use fossick_core::perm::{AclEntry, encode_acl, tag};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("shared")).unwrap();
    let acl = [
        AclEntry {
            tag: tag::USER_OBJ,
            perm: 7,
            id: 0,
        },
        AclEntry {
            tag: tag::USER,
            perm: 5,
            id: 4242,
        },
        AclEntry {
            tag: tag::GROUP_OBJ,
            perm: 5,
            id: 0,
        },
        AclEntry {
            tag: tag::MASK,
            perm: 5,
            id: 0,
        },
        AclEntry {
            tag: tag::OTHER,
            perm: 0,
            id: 0,
        },
    ];
    if let Err(e) = rustix::fs::setxattr(
        root.join("shared"),
        fossick_core::perm::ACL_XATTR,
        &encode_acl(&acl),
        rustix::fs::XattrFlags::empty(),
    ) {
        eprintln!("skipping: this filesystem has no POSIX ACLs ({e})");
        return;
    }
    let opts = ScanOptions {
        read_acls: true,
        capture_handles: true,
        ..ScanOptions::default()
    };
    let mut handles = 0;
    let mut acls = Vec::new();
    scan(root, &opts, |l| {
        handles += usize::from(l.handle.is_some());
        if let Some(a) = l.acl {
            acls.push(fossick_core::perm::parse_acl(&a).unwrap());
        }
    })
    .unwrap();
    assert_eq!(acls.len(), 1);
    // The kernel may reorder entries; compare as sets.
    let mut got = acls[0].clone();
    let mut want = acl.to_vec();
    got.sort_by_key(|e| (e.tag, e.id));
    want.sort_by_key(|e| (e.tag, e.id));
    assert_eq!(got, want);
    if handles == 0 {
        eprintln!("file handles not supported on this filesystem");
    } else {
        assert_eq!(handles, 2);
    }
    // Through the applier the ACL lands in the index.
    let mut w = IndexWriter::new(IndexConfig::default());
    let (meta, _, _) = stat_path(root).unwrap();
    let (_, r) = w
        .add_volume("t", root.as_os_str().as_bytes(), "t", None, &meta)
        .unwrap();
    scan_into(root, &mut w, r, &opts).unwrap();
    let g = w.publish();
    let shared = w.lookup(r, b"shared").unwrap();
    assert_eq!(g.acl(shared).map(<[_]>::len), Some(5));
    assert_eq!(g.restricted_count(shared), Some(1));
}

#[test]
fn inline_scan_matches_parallel_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    make_tree(root);
    let (w, _) = build(root);
    let mut w2 = IndexWriter::new(IndexConfig::default());
    let (meta, _, _) = stat_path(root).unwrap();
    let (_, r) = w2
        .add_volume("test", root.as_os_str().as_bytes(), "test", None, &meta)
        .unwrap();
    let mut applier = Applier::new(r, false);
    let mut order = Vec::new();
    let mut opened = 0;
    let stats = scan_inline(root, &ScanOptions::default(), None, |v| match v {
        Visit::Opening(..) => opened += 1,
        Visit::Listed(l) => {
            order.push(l.dir);
            applier.apply(&mut w2, l);
        }
    })
    .unwrap();
    w2.publish();
    assert_eq!(stats.errors, 0);
    assert_eq!(indexed(&w, root), indexed(&w2, root));
    assert_eq!(order[0], 0, "the root comes first");
    assert_eq!(opened, stats.dirs);
}

#[test]
fn racy_directory_times_are_not_trusted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    make_tree(root);
    // Built right after the tree was made: every directory is racy, so a pruned rescan reads
    // them all again.
    let (mut w, stats) = build(root);
    let g = w.handle().load();
    let prior = IndexPrior::new(Arc::clone(&g), g.volumes()[0].root, i64::MAX);
    assert_eq!(rescan(&mut w, root, Some(&prior)).unchanged_dirs, 0);
    assert!(stats.dirs > 0);
}
