use super::*;
use crate::index::{EntryId, IndexConfig, IndexWriter, TimeField};
use crate::meta::{EntryMeta, Timestamp};

fn sample(track_atime: bool) -> IndexWriter {
    let mut w = IndexWriter::new(IndexConfig { track_atime });
    let (_, root) = w
        .add_volume(
            "uuid-1",
            b"/data",
            "ext4",
            Some("Data"),
            &EntryMeta::dir(1_600_000_000),
        )
        .unwrap();
    let docs = w
        .insert(root, b"docs", &EntryMeta::dir(1_600_000_100))
        .unwrap();
    for i in 0..9000u64 {
        let mut m = EntryMeta::file(i * 13, 1_600_000_000 + i as i64);
        m.uid = 1000 + (i % 3) as u32;
        m.atime = Some(Timestamp::from_secs(1_700_000_000));
        let name = format!("file-{i}.txt");
        w.insert(docs, name.as_bytes(), &m).unwrap();
    }
    let mut big = EntryMeta::file(1 << 40, 1_600_000_000);
    big.btime = Some(Timestamp::from_secs(-1000));
    w.insert(root, "größe\u{FF}.bin".as_bytes(), &big).unwrap();
    w.insert(root, b"bad\xFFname", &EntryMeta::file(1, 1))
        .unwrap();
    w.publish();
    w
}

fn assert_same(a: &Generation, b: &Generation) {
    assert_eq!(a.len(), b.len());
    assert_eq!(a.number(), b.number());
    assert_eq!(a.volumes(), b.volumes());
    let ids: Vec<EntryId> = a.iter().collect();
    assert_eq!(ids, b.iter().collect::<Vec<_>>());
    for id in ids {
        assert_eq!(a.name(id), b.name(id));
        assert_eq!(a.path(id), b.path(id));
        assert_eq!(a.size(id), b.size(id));
        assert_eq!(a.kind(id), b.kind(id));
        assert_eq!(a.owner(id), b.owner(id));
        for f in [
            TimeField::Modified,
            TimeField::Changed,
            TimeField::Created,
            TimeField::Accessed,
        ] {
            assert_eq!(a.time(id, f), b.time(id, f));
        }
    }
    for k in crate::index::OrderKind::ALL {
        assert_eq!(
            a.orders().get(k).iter().collect::<Vec<_>>(),
            b.orders().get(k).iter().collect::<Vec<_>>()
        );
    }
}

#[test]
fn roundtrip_through_file_and_continue_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.fsidx");
    for atime in [false, true] {
        let w = sample(atime);
        let g = w.handle().load();
        write(&g, &path).unwrap();
        let mode = std::fs::metadata(&path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.permissions().mode() & 0o777, 0o600);
        for verify in [Verify::All, Verify::Essential] {
            let snap = load(&path, verify).unwrap();
            snap.verify_deferred().unwrap();
            assert_same(&g, &snap.generation);
            // Resume writing on top of the mapped snapshot.
            let mut w2 =
                IndexWriter::from_generation(&snap.generation, IndexConfig { track_atime: atime });
            let root = snap.generation.volumes()[0].root;
            let docs = w2.lookup(root, b"docs").unwrap();
            w2.insert(docs, b"new.txt", &EntryMeta::file(5, 5)).unwrap();
            let f = w2.lookup(docs, b"file-7.txt").unwrap();
            w2.rename(f, root, b"moved.txt").unwrap();
            w2.remove(w2.lookup(docs, b"file-8.txt").unwrap()).unwrap();
            let g2 = w2.publish();
            w2.check_invariants().unwrap();
            assert_eq!(g2.len(), g.len());
            // The mapped generation is untouched.
            assert_same(&g, &snap.generation);
        }
    }
    // No temp files are left behind.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(leftovers.len(), 1);
}

#[test]
fn detects_corruption_and_bad_files() {
    let w = sample(false);
    let g = w.handle().load();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("i.fsidx");
    write(&g, &path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(load_bytes(&bytes, Verify::All).is_ok());

    assert!(matches!(
        load_bytes(b"short", Verify::All),
        Err(PersistError::BadMagic)
    ));
    let mut bad = bytes.clone();
    bad[0] = b'X';
    assert!(matches!(
        load_bytes(&bad, Verify::All),
        Err(PersistError::BadMagic)
    ));
    let mut bad = bytes.clone();
    bad[8] = 99;
    assert!(matches!(
        load_bytes(&bad, Verify::All),
        Err(PersistError::Incompatible { found: 99 })
    ));
    // Flip one byte in every 4 KiB stretch of the body: every flip must be detected, either at
    // load or by the deferred verification. Nothing may panic.
    let mut detected = 0;
    let mut checked = 0;
    let mut at = HEADER_LEN as usize;
    while at < bytes.len() {
        let mut bad = bytes.clone();
        bad[at] ^= 0x5A;
        checked += 1;
        match load_bytes(&bad, Verify::Essential) {
            Err(_) => detected += 1,
            Ok(s) => {
                if s.verify_deferred().is_err() {
                    detected += 1;
                }
            }
        }
        at += 4093;
    }
    // Padding bytes between sections are not covered by checksums; everything else is.
    assert!(
        detected * 10 >= checked * 9,
        "detected {detected} of {checked}"
    );
    // Truncation.
    assert!(load_bytes(&bytes[..bytes.len() / 2], Verify::All).is_err());
}

#[test]
fn corrupt_section_values_are_rejected_without_panicking() {
    // Regression for a fuzzer finding: a name index entry near u64::MAX overflowed an addition.
    let w = sample(false);
    let g = w.handle().load();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("i.fsidx");
    write(&g, &path).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let header = Header::read_from_bytes(&bytes[..HEADER_LEN as usize]).unwrap();
    let table_len = header.sections as usize * 32;
    let table_start = header.table_offset as usize;
    let table = <[SectionEntry]>::ref_from_bytes(&bytes[table_start..table_start + table_len])
        .unwrap()
        .to_vec();
    let idx = table
        .iter()
        .find(|s| s.kind == Kind::NameIndex as u16)
        .unwrap();
    let at = idx.offset as usize;
    bytes[at..at + 8].copy_from_slice(&(u64::MAX - 1).to_le_bytes());
    assert!(matches!(
        load_bytes(&bytes, Verify::Unchecked),
        Err(PersistError::Corrupt(_))
    ));
    // With checksums on, the same corruption is a checksum error.
    assert!(matches!(
        load_bytes(&bytes, Verify::All),
        Err(PersistError::Checksum(_))
    ));
}
