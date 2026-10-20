use super::*;
use crate::meta::{EntryMeta, S_IFDIR};

fn file(size: u64) -> EntryMeta {
    EntryMeta::file(size, 1_700_000_000)
}

fn dir() -> EntryMeta {
    EntryMeta::dir(1_700_000_000)
}

fn setup() -> (IndexWriter, EntryId) {
    let mut w = IndexWriter::new(IndexConfig::default());
    let (_, root) = w
        .add_volume("test", b"/data", "ext4", None, &dir())
        .unwrap();
    (w, root)
}

fn s(p: Vec<u8>) -> String {
    String::from_utf8(p).unwrap()
}

#[test]
fn listing_times_are_separate_from_displayed_times() {
    let (mut w, root) = setup();
    let d = w.insert(root, b"z", &dir()).unwrap();
    w.insert(root, b"a", &dir()).unwrap();
    let g = w.publish();
    assert_eq!(g.dir_times_ns(d), None, "not listed yet");
    // Recording listing times must not move the directory in the date order.
    w.set_dir_times(d, Some((5, 6))).unwrap();
    let g = w.publish();
    w.check_invariants().unwrap();
    assert_eq!(g.dir_times_ns(d), Some((5, 6)));
    assert_eq!(
        g.time(d, TimeField::Modified),
        Some(Timestamp::from_secs(1_700_000_000))
    );
    // Metadata updates change the displayed time and keep the listing times.
    let mut newer = dir();
    newer.mtime = Some(Timestamp::from_secs(1_800_000_000));
    w.update(d, &newer).unwrap();
    let g = w.publish();
    w.check_invariants().unwrap();
    assert_eq!(g.dir_times_ns(d), Some((5, 6)));
    assert_eq!(
        g.time(d, TimeField::Modified),
        Some(Timestamp::from_secs(1_800_000_000))
    );
    w.set_dir_times(d, None).unwrap();
    let g = w.publish();
    w.check_invariants().unwrap();
    assert_eq!(g.dir_times_ns(d), None);
}

#[test]
fn build_and_read_back() {
    let (mut w, root) = setup();
    let docs = w.insert(root, b"docs", &dir()).unwrap();
    let a = w.insert(docs, b"a.txt", &file(100)).unwrap();
    let b = w.insert(docs, b"B.txt", &file(50)).unwrap();
    let pics = w.insert(root, b"pics", &dir()).unwrap();
    let c = w.insert(pics, b"cat.jpg", &file(1000)).unwrap();
    let g = w.publish();
    w.check_invariants().unwrap();
    assert_eq!(g.len(), 6);
    assert_eq!(s(g.path(a)), "/data/docs/a.txt");
    assert_eq!(s(g.parent_path(a)), "/data/docs");
    assert_eq!(s(g.path(root)), "/data");
    assert_eq!(g.size(docs), 150);
    assert_eq!(g.size(root), 1150);
    assert_eq!(g.child_counts(root), Some((0, 2)));
    assert_eq!(g.child_counts(docs), Some((2, 0)));
    assert_eq!(g.depth(c), 3);
    // Name order: data, docs, a.txt? -> collation: a.txt, B.txt, cat.jpg, data, docs, pics
    let names: Vec<String> = g
        .orders()
        .name
        .iter()
        .map(|i| s(g.name(EntryId(i)).to_vec()))
        .collect();
    assert_eq!(names, ["a.txt", "B.txt", "cat.jpg", "data", "docs", "pics"]);
    // Path order: root first, then children of /data, then of docs, then of pics.
    let paths: Vec<String> = g
        .orders()
        .path
        .iter()
        .map(|i| s(g.path(EntryId(i))))
        .collect();
    assert_eq!(
        paths,
        [
            "/data",
            "/data/docs",
            "/data/pics",
            "/data/docs/a.txt",
            "/data/docs/B.txt",
            "/data/pics/cat.jpg"
        ]
    );
    let sizes: Vec<u64> = g.orders().size.iter().map(|i| g.size(EntryId(i))).collect();
    assert!(sizes.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(w.lookup(docs, b"a.txt"), Some(a));
    assert_eq!(w.lookup(docs, b"A.TXT"), None);
    assert!(g.is_live(b));
}

#[test]
fn duplicate_names_rejected_and_upsert_updates() {
    let (mut w, root) = setup();
    let f = w.insert(root, b"x", &file(1)).unwrap();
    assert_eq!(w.insert(root, b"x", &file(2)), Err(IndexError::Exists(f)));
    assert_eq!(
        w.insert(root, b"a/b", &file(2)),
        Err(IndexError::InvalidName)
    );
    let f2 = w.upsert(root, b"x", &file(7)).unwrap();
    assert_eq!(f, f2);
    w.publish();
    w.check_invariants().unwrap();
    assert_eq!(w.size(root), 7);
    // Kind change through upsert replaces the entry.
    let d = w.upsert(root, b"x", &dir()).unwrap();
    assert_ne!(d, f);
    let g = w.publish();
    w.check_invariants().unwrap();
    assert_eq!(g.kind(d), FileKind::Dir);
    assert_eq!(g.size(root), 0);
}

#[test]
fn rename_move_and_remove_keep_invariants() {
    let (mut w, root) = setup();
    let a = w.insert(root, b"a", &dir()).unwrap();
    let b = w.insert(root, b"b", &dir()).unwrap();
    let a1 = w.insert(a, b"one", &dir()).unwrap();
    let f = w.insert(a1, b"f.bin", &file(10)).unwrap();
    w.insert(b, b"g.bin", &file(5)).unwrap();
    w.publish();
    w.check_invariants().unwrap();

    // Move a/one under b.
    w.rename(a1, b, b"moved").unwrap();
    let g = w.publish();
    w.check_invariants().unwrap();
    assert_eq!(s(g.path(f)), "/data/b/moved/f.bin");
    assert_eq!(g.size(a), 0);
    assert_eq!(g.size(b), 15);

    // Cycle detection.
    assert_eq!(w.rename(b, a1, b"loop"), Err(IndexError::WouldCycle));
    // Rename a file within its directory.
    w.rename(f, a1, b"renamed.bin").unwrap();
    let g = w.publish();
    w.check_invariants().unwrap();
    assert_eq!(s(g.path(f)), "/data/b/moved/renamed.bin");
    assert_eq!(w.lookup(a1, b"f.bin"), None);
    assert_eq!(w.lookup(a1, b"renamed.bin"), Some(f));

    // Remove the subtree.
    assert_eq!(w.remove(a1).unwrap(), 2);
    let g = w.publish();
    w.check_invariants().unwrap();
    assert!(!g.is_live(f));
    assert_eq!(g.size(b), 5);
    assert_eq!(g.len(), (5 - 1));
    assert_eq!(g.changes().removed.len(), 2);
}

#[test]
fn generations_are_isolated() {
    let (mut w, root) = setup();
    let f = w.insert(root, b"f", &file(1)).unwrap();
    let g1 = w.publish();
    w.update(f, &file(99)).unwrap();
    w.rename(f, root, b"g").unwrap();
    let g2 = w.publish();
    assert_eq!(g1.size(f), 1);
    assert_eq!(g1.name(f), b"f");
    assert_eq!(g2.size(f), 99);
    assert_eq!(g2.name(f), b"g");
    assert_eq!(g2.changes().modified, vec![root, f]);
}

#[test]
fn sequential_inserts_exhaust_gaps_and_relabel() {
    let (mut w, root) = setup();
    // Seed a few names, then append names that always land in the same gap.
    w.insert(root, b"a", &file(1)).unwrap();
    w.insert(root, b"z", &file(1)).unwrap();
    w.publish();
    for i in 0..600 {
        let name = format!("m{i:05}");
        w.insert(root, name.as_bytes(), &file(1)).unwrap();
        if i % 3 == 0 {
            w.publish();
        }
    }
    w.publish();
    w.check_invariants().unwrap();
    // Same for directories (dir labels).
    let mut parent = root;
    for i in 0..300 {
        let name = format!("d{i:04}");
        let d = w.insert(parent, name.as_bytes(), &dir()).unwrap();
        if i % 2 == 0 {
            parent = d;
        }
        w.publish();
    }
    w.check_invariants().unwrap();
}

#[test]
fn many_entries_across_pages_and_slot_reuse() {
    let (mut w, root) = setup();
    let mut ids = Vec::new();
    for i in 0..10_000 {
        ids.push(
            w.insert(root, format!("n{i}").as_bytes(), &file(i))
                .unwrap(),
        );
    }
    w.publish();
    w.check_invariants().unwrap();
    for &id in ids.iter().step_by(2) {
        w.remove(id).unwrap();
    }
    w.publish();
    w.check_invariants().unwrap();
    // Freed slots are reused.
    let again = w.insert(root, b"again", &file(3)).unwrap();
    assert!(ids.contains(&again));
    let g = w.publish();
    w.check_invariants().unwrap();
    assert_eq!(g.len(), 1 + 5000 + 1);
    let expected: u64 = (1..10_000u64).step_by(2).sum::<u64>() + 3;
    assert_eq!(g.size(root), expected);
}

#[test]
fn non_ascii_and_invalid_names() {
    let (mut w, root) = setup();
    let a = w.insert(root, "Ärger.txt".as_bytes(), &file(1)).unwrap();
    let b = w.insert(root, b"bad\xFFname", &file(1)).unwrap();
    let c = w
        .insert(root, "\u{212A}elvin".as_bytes(), &file(1))
        .unwrap();
    let g = w.publish();
    w.check_invariants().unwrap();
    assert_ne!(g.flags(a) & flags::NON_ASCII, 0);
    assert_eq!(g.name(b), b"bad\xFFname");
    assert_ne!(g.flags(c) & flags::FOLD_SPECIAL, 0);
}

#[test]
fn big_sizes_and_odd_times_use_side_tables() {
    let (mut w, root) = setup();
    let mut m = file(10 * 1024 * 1024 * 1024);
    m.mtime = Some(Timestamp::from_secs(-86_400));
    m.btime = Some(Timestamp::from_secs(5_000_000_000));
    let f = w.insert(root, b"huge.img", &m).unwrap();
    let g = w.publish();
    assert_eq!(g.size(f), 10 * 1024 * 1024 * 1024);
    assert_eq!(g.size(root), 10 * 1024 * 1024 * 1024);
    assert_eq!(
        g.time(f, TimeField::Modified),
        Some(Timestamp::from_secs(-86_400))
    );
    assert_eq!(
        g.time(f, TimeField::Created),
        Some(Timestamp::from_secs(5_000_000_000))
    );
    w.check_invariants().unwrap();
    w.update(f, &file(5)).unwrap();
    let g = w.publish();
    assert_eq!(g.size(f), 5);
    assert_eq!(g.size(root), 5);
}

#[test]
fn restricted_counter_tracks_modes() {
    let (mut w, root) = setup();
    let mut private = dir();
    private.mode = S_IFDIR | 0o700;
    let p = w.insert(root, b"private", &private).unwrap();
    let q = w.insert(p, b"inner", &dir()).unwrap();
    let g = w.publish();
    assert_eq!(g.restricted_count(root), Some(1));
    assert_eq!(g.restricted_count(p), Some(1));
    assert_eq!(g.restricted_count(q), Some(0));
    w.update(p, &dir()).unwrap();
    let g = w.publish();
    assert_eq!(g.restricted_count(root), Some(0));
    w.update(q, &private).unwrap();
    let g = w.publish();
    assert_eq!(g.restricted_count(root), Some(1));
    w.remove(p).unwrap();
    let g = w.publish();
    assert_eq!(g.restricted_count(root), Some(0));
}

#[test]
fn nested_volumes_sort_by_full_path() {
    let mut w = IndexWriter::new(IndexConfig::default());
    let (_, r) = w.add_volume("root", b"/", "ext4", None, &dir()).unwrap();
    let (_, h) = w
        .add_volume("home", b"/home", "btrfs", None, &dir())
        .unwrap();
    let etc = w.insert(r, b"etc", &dir()).unwrap();
    let zz = w.insert(r, b"zz", &dir()).unwrap();
    let u = w.insert(h, b"user", &dir()).unwrap();
    let f = w.insert(u, b"notes.md", &file(3)).unwrap();
    let g = w.publish();
    w.check_invariants().unwrap();
    assert_eq!(s(g.path(f)), "/home/user/notes.md");
    assert_eq!(s(g.path(etc)), "/etc");
    // dir_key order: / < /etc < /home < /home/user < /zz
    let key = |e: EntryId| g.data.dir_key_of_dir(g.data.dir_of(e.0).unwrap());
    assert!(key(r) < key(etc));
    assert!(key(etc) < key(h));
    assert!(key(h) < key(u));
    assert!(key(u) < key(zz));
    assert_eq!(g.size(r), 0, "folder sizes stop at mount points");
}

mod model {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    #[derive(Clone, Debug)]
    enum Op {
        Insert {
            dir: usize,
            name: u8,
            is_dir: bool,
            size: u16,
        },
        Remove {
            target: usize,
        },
        Rename {
            target: usize,
            dir: usize,
            name: u8,
        },
        Resize {
            target: usize,
            size: u16,
        },
        Publish,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (any::<usize>(), 0u8..12, any::<bool>(), any::<u16>())
                .prop_map(|(dir, name, is_dir, size)| Op::Insert { dir, name, is_dir, size }),
            1 => any::<usize>().prop_map(|target| Op::Remove { target }),
            2 => (any::<usize>(), any::<usize>(), 0u8..12)
                .prop_map(|(target, dir, name)| Op::Rename { target, dir, name }),
            1 => (any::<usize>(), any::<u16>()).prop_map(|(target, size)| Op::Resize { target, size }),
            1 => Just(Op::Publish),
        ]
    }

    const NAMES: [&str; 12] = [
        "a", "B", "c.txt", "C.TXT", "ä", "Ä", "z", "aa", "a.b", "x\u{301}", "K", "\u{212A}",
    ];

    /// Model: path (components) -> (is_dir, size, id).
    type Model = BTreeMap<Vec<Vec<u8>>, (bool, u64, EntryId)>;

    fn check(w: &IndexWriter, model: &Model) {
        w.check_invariants().unwrap();
        let g = w.handle().load();
        assert_eq!(g.len(), model.len() + 1, "live count");
        for (path, &(is_dir, size, id)) in model {
            let mut p = b"/data".to_vec();
            for c in path {
                p.push(b'/');
                p.extend_from_slice(c);
            }
            assert_eq!(g.path(id), p);
            assert_eq!(g.kind(id).is_dir(), is_dir);
            if is_dir {
                let expect: u64 = model
                    .iter()
                    .filter(|(q, v)| q.len() > path.len() && q.starts_with(path) && !v.0)
                    .map(|(_, v)| v.1)
                    .sum();
                assert_eq!(
                    g.size(id),
                    expect,
                    "folder size of {:?}",
                    String::from_utf8_lossy(&p)
                );
            } else {
                assert_eq!(g.size(id), size);
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]
        #[test]
        fn random_mutations_match_model(ops in proptest::collection::vec(op(), 1..120)) {
            let (mut w, root) = setup();
            let mut model: Model = BTreeMap::new();
            let dirs = |m: &Model| -> Vec<(Vec<Vec<u8>>, EntryId)> {
                let mut v = vec![(Vec::new(), root)];
                v.extend(m.iter().filter(|(_, x)| x.0).map(|(p, x)| (p.clone(), x.2)));
                v
            };
            for op in ops {
                match op {
                    Op::Insert { dir: di, name, is_dir, size } => {
                        let ds = dirs(&model);
                        let (dpath, did) = ds[di % ds.len()].clone();
                        let name = NAMES[name as usize].as_bytes().to_vec();
                        let mut path = dpath.clone();
                        path.push(name.clone());
                        let meta = if is_dir { dir() } else { file(u64::from(size)) };
                        let r = w.insert(did, &name, &meta);
                        if let Some(&(_, _, existing)) = model.get(&path) {
                            prop_assert_eq!(r, Err(IndexError::Exists(existing)));
                        } else {
                            let id = r.unwrap();
                            model.insert(path, (is_dir, if is_dir { 0 } else { u64::from(size) }, id));
                        }
                    }
                    Op::Remove { target } => {
                        if model.is_empty() { continue; }
                        let keys: Vec<_> = model.keys().cloned().collect();
                        let path = keys[target % keys.len()].clone();
                        let id = model[&path].2;
                        let n = w.remove(id).unwrap();
                        let before = model.len();
                        model.retain(|p, _| !p.starts_with(&path));
                        prop_assert_eq!(n, before - model.len());
                    }
                    Op::Rename { target, dir: di, name } => {
                        if model.is_empty() { continue; }
                        let keys: Vec<_> = model.keys().cloned().collect();
                        let src = keys[target % keys.len()].clone();
                        let id = model[&src].2;
                        let ds = dirs(&model);
                        let (dpath, did) = ds[di % ds.len()].clone();
                        let name = NAMES[name as usize].as_bytes().to_vec();
                        let mut dst = dpath.clone();
                        dst.push(name.clone());
                        let r = w.rename(id, did, &name);
                        if dpath.starts_with(&src) && model[&src].0 {
                            if dst == src { prop_assert_eq!(r, Ok(())); }
                            else { prop_assert_eq!(r, Err(IndexError::WouldCycle)); }
                            continue;
                        }
                        if let Some(&(_, _, existing)) = model.get(&dst)
                            && existing != id {
                                prop_assert_eq!(r, Err(IndexError::Exists(existing)));
                                continue;
                            }
                        prop_assert_eq!(r, Ok(()));
                        let moved: Vec<_> = model
                            .iter()
                            .filter(|(p, _)| p.starts_with(&src))
                            .map(|(p, v)| (p.clone(), *v))
                            .collect();
                        for (p, _) in &moved { model.remove(p); }
                        for (p, v) in moved {
                            let mut np = dst.clone();
                            np.extend_from_slice(&p[src.len()..]);
                            model.insert(np, v);
                        }
                    }
                    Op::Resize { target, size } => {
                        let files: Vec<_> = model.iter().filter(|(_, v)| !v.0).map(|(p, _)| p.clone()).collect();
                        if files.is_empty() { continue; }
                        let path = files[target % files.len()].clone();
                        let id = model[&path].2;
                        w.update(id, &file(u64::from(size))).unwrap();
                        model.get_mut(&path).unwrap().1 = u64::from(size);
                    }
                    Op::Publish => {
                        w.publish();
                        check(&w, &model);
                    }
                }
            }
            w.publish();
            check(&w, &model);
        }
    }
}

#[test]
fn path_lookups_and_moving_volumes() {
    let mut w = IndexWriter::new(IndexConfig::default());
    let (vid, root) = w
        .add_volume("usb", b"/media/old", "exfat", None, &EntryMeta::dir(1))
        .unwrap();
    let (_, sys) = w
        .add_volume("sys", b"/", "ext4", None, &EntryMeta::dir(1))
        .unwrap();
    let media = w.insert(sys, b"media", &EntryMeta::dir(1)).unwrap();
    let d = w.insert(root, b"Docs", &EntryMeta::dir(2)).unwrap();
    let f = w.insert(d, b"a.txt", &EntryMeta::file(3, 3)).unwrap();
    w.insert(d, b"A.txt", &EntryMeta::file(3, 3)).unwrap();
    let g = w.publish();
    assert_eq!(w.lookup_path(b"/media/old/Docs/a.txt"), Some(f));
    assert_eq!(w.lookup_path(b"/media"), Some(media));
    assert_eq!(w.lookup_path(b"/media/old"), Some(root));
    assert_eq!(w.lookup_path(b"/media/old/nope"), None);
    assert_eq!(g.lookup_path(b"/media/old/Docs/a.txt"), Some(f));
    assert_eq!(g.lookup_path(b"/media/old"), Some(root));
    assert_eq!(g.lookup_path(b"/media/old/Docs/a.TXT"), None);
    assert_eq!(g.lookup_path(b"/"), Some(sys));
    w.set_volume_mount(vid, b"/run/media/new").unwrap();
    let g = w.publish();
    w.check_invariants().unwrap();
    assert_eq!(g.path(f), b"/run/media/new/Docs/a.txt");
    assert_eq!(g.name(root), b"new");
    assert_eq!(w.lookup_path(b"/run/media/new/Docs/a.txt"), Some(f));
    assert_eq!(g.lookup_path(b"/run/media/new/Docs/a.txt"), Some(f));
    assert_eq!(g.lookup_path(b"/media/old/Docs/a.txt"), None);
    // The path order follows the move: /run/media/new sorts after /media.
    let paths: Vec<Vec<u8>> = g
        .orders()
        .get(OrderKind::Path)
        .iter()
        .map(|id| g.path(EntryId(id)))
        .collect();
    assert_eq!(paths.len(), g.len());
    let pos = |p: &[u8]| paths.iter().position(|x| x == p).unwrap();
    assert!(pos(b"/media") < pos(b"/run/media/new"));
    assert!(pos(b"/run/media/new") < pos(b"/run/media/new/Docs/a.txt"));
}
