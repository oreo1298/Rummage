use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::*;
use crate::index::{EntryId, IndexConfig, IndexWriter};
use crate::meta::{EntryMeta, S_IFDIR, S_IFREG};
use crate::query::{Clock, CompileContext, Macros, NumericIds, SearchOptions, compile_str};
use crate::search::{SearchContext, Sort, SortField, search};

fn e(tag: u16, perm: u16, id: u32) -> AclEntry {
    AclEntry { tag, perm, id }
}

#[test]
fn acl_xattr_round_trip_and_rejects_garbage() {
    let acl = vec![
        e(tag::USER_OBJ, 7, 0),
        e(tag::USER, 0, 2000),
        e(tag::GROUP_OBJ, 5, 0),
        e(tag::MASK, 5, 0),
        e(tag::OTHER, 5, 0),
    ];
    assert_eq!(parse_acl(&encode_acl(&acl)), Some(acl));
    assert_eq!(parse_acl(&[]), None);
    assert_eq!(parse_acl(&[1, 0, 0, 0]), None, "wrong version");
    assert_eq!(parse_acl(&[2, 0, 0, 0, 1, 0]), None, "truncated entry");
    assert_eq!(
        parse_acl(&encode_acl(&[e(0x40, 7, 0)])),
        None,
        "unknown tag"
    );
    assert_eq!(
        parse_acl(&encode_acl(&[e(tag::USER, 9, 0)])),
        None,
        "bad perm"
    );
}

#[test]
fn mode_bits() {
    let alice = Creds::new(1000, 100, &[]);
    let bob = Creds::new(2000, 200, &[50]);
    // Owner bits apply to the owner even when group/other are more permissive.
    assert!(!allowed(&alice, R, 0o077, 1000, 50, None));
    assert!(allowed(&alice, R | X, 0o700, 1000, 50, None));
    assert!(
        allowed(&bob, R | X, 0o750, 1000, 50, None),
        "supplementary group"
    );
    assert!(
        !allowed(&bob, R | X, 0o705, 1000, 50, None),
        "group bits, not other"
    );
    assert!(!allowed(&bob, R | X, 0o711, 1000, 100, None));
    assert!(allowed(&bob, X, 0o711, 1000, 100, None));
    assert!(allowed(&Creds::root(), R | W | X, 0, 1000, 100, None));
}

#[test]
fn acl_algorithm() {
    let bob = Creds::new(2000, 200, &[50, 60]);
    let carol = Creds::new(3000, 300, &[]);
    let base = |extra: &[AclEntry]| {
        let mut v = vec![
            e(tag::USER_OBJ, 7, 0),
            e(tag::GROUP_OBJ, 5, 0),
            e(tag::OTHER, 5, 0),
            e(tag::MASK, 7, 0),
        ];
        v.extend_from_slice(extra);
        v
    };
    // A named user entry overrides “other”.
    let acl = base(&[e(tag::USER, 0, 2000)]);
    assert!(!allowed(&bob, R | X, 0o755, 1000, 100, Some(&acl)));
    assert!(allowed(&carol, R | X, 0o755, 1000, 100, Some(&acl)));
    // The mask limits named entries.
    let mut acl = base(&[e(tag::USER, 7, 2000)]);
    acl.iter_mut()
        .filter(|x| x.tag == tag::MASK)
        .for_each(|x| x.perm = 4);
    assert!(!allowed(&bob, R | X, 0o745, 1000, 100, Some(&acl)));
    assert!(allowed(&bob, R, 0o745, 1000, 100, Some(&acl)));
    // Group class: one matching entry must grant everything; a match blocks “other”.
    let acl = base(&[e(tag::GROUP, 4, 50), e(tag::GROUP, 1, 60)]);
    assert!(!allowed(&bob, R | X, 0o755, 1000, 100, Some(&acl)));
    assert!(allowed(&bob, R, 0o755, 1000, 100, Some(&acl)));
    let acl = base(&[e(tag::GROUP, 0, 50)]);
    assert!(
        !allowed(&bob, R, 0o755, 1000, 100, Some(&acl)),
        "matched group denies"
    );
}

/// /v (0755)
///   pub/ 0755          a.txt
///   home/ 0711         alice/ 0755 (uid 1000) → notes
///   priv/ 0700 uid 1000 → secret
///   grp/ 0750 gid 50   → g.txt
///   acl/ 0755 + ACL user:2000:--- → x.txt
fn tree() -> (Arc<crate::index::Generation>, Vec<(&'static str, EntryId)>) {
    let mut w = IndexWriter::new(IndexConfig::default());
    let dir = |mode: u32, uid: u32, gid: u32| EntryMeta {
        mode: S_IFDIR | mode,
        uid,
        gid,
        ..EntryMeta::default()
    };
    let file = EntryMeta {
        mode: S_IFREG | 0o644,
        size: 10,
        ..EntryMeta::default()
    };
    let (_, root) = w
        .add_volume("v", b"/v", "ext4", None, &dir(0o755, 0, 0))
        .unwrap();
    let mut ids = Vec::new();
    let mut add = |w: &mut IndexWriter, parent, name: &'static str, meta: &EntryMeta| {
        let id = w.insert(parent, name.as_bytes(), meta).unwrap();
        ids.push((name, id));
        id
    };
    let pub_ = add(&mut w, root, "pub", &dir(0o755, 0, 0));
    add(&mut w, pub_, "a.txt", &file);
    let home = add(&mut w, root, "home", &dir(0o711, 0, 0));
    let alice = add(&mut w, home, "alice", &dir(0o755, 1000, 100));
    add(&mut w, alice, "notes", &file);
    let prv = add(&mut w, root, "priv", &dir(0o700, 1000, 100));
    add(&mut w, prv, "secret", &file);
    let grp = add(&mut w, root, "grp", &dir(0o750, 0, 50));
    add(&mut w, grp, "g.txt", &file);
    let acl = add(&mut w, root, "acl", &dir(0o755, 0, 0));
    add(&mut w, acl, "x.txt", &file);
    w.set_acl(
        acl,
        Some(&[
            e(tag::USER_OBJ, 7, 0),
            e(tag::USER, 0, 2000),
            e(tag::GROUP_OBJ, 5, 0),
            e(tag::MASK, 5, 0),
            e(tag::OTHER, 5, 0),
        ]),
    )
    .unwrap();
    let g = w.publish();
    w.check_invariants().unwrap();
    (g, ids)
}

fn visible_names(
    g: &crate::index::Generation,
    ids: &[(&'static str, EntryId)],
    v: &Visibility,
) -> Vec<&'static str> {
    let mut out: Vec<&str> = ids
        .iter()
        .filter(|(_, id)| v.entries.get(id.0))
        .map(|(n, _)| *n)
        .collect();
    out.sort_unstable();
    assert!(v.entries.get(g.volumes()[0].root.0), "volume root visible");
    out
}

#[test]
fn visibility_requires_every_ancestor_listable() {
    let (g, ids) = tree();
    let all = |_: &Volume| true;
    let bob = visibility(&g, &Creds::new(2000, 200, &[50]), &all);
    assert_eq!(
        visible_names(&g, &ids, &bob),
        ["a.txt", "acl", "g.txt", "grp", "home", "priv", "pub"]
    );
    let carol = visibility(&g, &Creds::new(3000, 300, &[]), &all);
    assert_eq!(
        visible_names(&g, &ids, &carol),
        ["a.txt", "acl", "grp", "home", "priv", "pub", "x.txt"]
    );
    // alice owns priv but can’t list /v/home (0711), so her own home stays hidden too.
    let alice = visibility(&g, &Creds::new(1000, 100, &[]), &all);
    assert_eq!(
        visible_names(&g, &ids, &alice),
        [
            "a.txt", "acl", "grp", "home", "priv", "pub", "secret", "x.txt"
        ]
    );
    let root = visibility(&g, &Creds::root(), &all);
    assert_eq!(root.count(), g.len());
    // Listable directories: carol may list pub and acl but not home, priv or grp.
    let id = |n: &str| ids.iter().find(|(x, _)| *x == n).unwrap().1;
    assert!(carol.listable.get(id("pub").0) && carol.listable.get(id("acl").0));
    assert!(!carol.listable.get(id("home").0) && !carol.listable.get(id("grp").0));
    // A hidden mount point hides the whole volume.
    let none = visibility(&g, &Creds::new(3000, 300, &[]), &|_: &Volume| false);
    assert_eq!(none.count(), 0);
}

#[test]
fn filtered_search_hides_counts_and_sizes() {
    let (g, _) = tree();
    let carol = visibility(&g, &Creds::new(3000, 300, &[]), &|_: &Volume| true);
    let run = |query: &str, sort: Sort| -> Vec<Vec<u8>> {
        let ctx = CompileContext {
            options: SearchOptions::default(),
            clock: Clock::at(0, jiff::tz::TimeZone::UTC),
            macros: &Macros::default(),
            users: &NumericIds,
        };
        let c = compile_str(query, &ctx).unwrap();
        let cancel = AtomicBool::new(false);
        let mut sctx = SearchContext::basic(&cancel);
        sctx.visible = Some(&carol.entries);
        sctx.listable = Some(&carol.listable);
        sctx.hide_restricted_sizes = true;
        search(&g, &c, sort, &sctx)
            .unwrap()
            .ids
            .iter()
            .map(|&id| g.path(id))
            .collect()
    };
    let by_name = Sort::default();
    // home holds alice, but carol can’t list home, so its child count is unknown to her.
    assert!(run("childfoldercount:1", by_name).is_empty());
    assert_eq!(
        run("childcount:1", by_name),
        [b"/v/acl".to_vec(), b"/v/pub".to_vec()]
    );
    // Sizes of folders holding restricted directories (here every folder but pub) are hidden.
    assert_eq!(run("folder: size:>0", by_name), [b"/v/pub".to_vec()]);
    // …and they sort last by size, even descending.
    let by_size = Sort {
        field: SortField::Size,
        descending: true,
    };
    let folders = run("folder:", by_size);
    assert_eq!(folders.len(), 6);
    assert_eq!(folders[0], b"/v/pub");
    // Without hiding, /v (the largest) would come first.
    let cancel = AtomicBool::new(false);
    let ctx = CompileContext {
        options: SearchOptions::default(),
        clock: Clock::at(0, jiff::tz::TimeZone::UTC),
        macros: &Macros::default(),
        users: &NumericIds,
    };
    let c = compile_str("folder:", &ctx).unwrap();
    let all = search(&g, &c, by_size, &SearchContext::basic(&cancel)).unwrap();
    assert_eq!(g.path(all.ids[0]), b"/v");
}
