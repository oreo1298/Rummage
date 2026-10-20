use std::sync::atomic::AtomicBool;

use jiff::tz::TimeZone;

use super::*;
use crate::index::{IndexConfig, IndexWriter};
use crate::meta::{EntryMeta, S_IFDIR, S_IFLNK, S_IFREG, Timestamp};
use crate::query::{Clock, CompileContext, Macros, NumericIds, SearchOptions, compile_str};

/// /t
///   docs/            (dir, 0755)
///     Report.PDF     100 B, 2024-06-12 10:00
///     report-old.pdf 5 MiB, 2023-01-01
///     notes.txt      1 KiB
///     Café.md        10 B
///   music/
///     song.mp3       4 MiB
///     song.flac      30 MiB
///   empty/
///   private/         (0700, uid 1000)
///     secret.key     1 B, uid 1000, mode 0600
///   bin/
///     run.sh         0755
///     link -> run.sh (symlink)
///   .hidden          dotfile
///   a b.txt          name with a space
///   bad\xFF.bin      not UTF-8
fn fixture() -> Arc<Generation> {
    let mut w = IndexWriter::new(IndexConfig::default());
    let t = |s: &str| {
        Some(Timestamp::from_secs(
            s.parse::<jiff::Timestamp>().unwrap().as_second(),
        ))
    };
    let dir = |mode: u32| EntryMeta {
        mode: S_IFDIR | mode,
        mtime: t("2024-01-01T00:00:00Z"),
        ..EntryMeta::default()
    };
    let file = |size: u64, when: &str| EntryMeta {
        mode: S_IFREG | 0o644,
        size,
        mtime: t(when),
        ctime: t(when),
        uid: 0,
        ..EntryMeta::default()
    };
    let (_, root) = w
        .add_volume("fixture", b"/t", "ext4", Some("Fixture"), &dir(0o755))
        .unwrap();
    let docs = w.insert(root, b"docs", &dir(0o755)).unwrap();
    w.insert(docs, b"Report.PDF", &file(100, "2024-06-12T10:00:00Z"))
        .unwrap();
    w.insert(
        docs,
        b"report-old.pdf",
        &file(5 << 20, "2023-01-01T00:00:00Z"),
    )
    .unwrap();
    w.insert(docs, b"notes.txt", &file(1024, "2024-06-10T00:00:00Z"))
        .unwrap();
    w.insert(
        docs,
        "Café.md".as_bytes(),
        &file(10, "2024-05-01T00:00:00Z"),
    )
    .unwrap();
    let music = w.insert(root, b"music", &dir(0o755)).unwrap();
    w.insert(music, b"song.mp3", &file(4 << 20, "2022-03-03T00:00:00Z"))
        .unwrap();
    w.insert(music, b"song.flac", &file(30 << 20, "2022-03-03T00:00:00Z"))
        .unwrap();
    w.insert(root, b"empty", &dir(0o755)).unwrap();
    let mut private = dir(0o700);
    private.uid = 1000;
    let p = w.insert(root, b"private", &private).unwrap();
    let mut key = file(1, "2024-06-01T00:00:00Z");
    key.mode = S_IFREG | 0o600;
    key.uid = 1000;
    w.insert(p, b"secret.key", &key).unwrap();
    let bin = w.insert(root, b"bin", &dir(0o755)).unwrap();
    let mut sh = file(50, "2024-06-01T00:00:00Z");
    sh.mode = S_IFREG | 0o755;
    w.insert(bin, b"run.sh", &sh).unwrap();
    let mut link = file(6, "2024-06-01T00:00:00Z");
    link.mode = S_IFLNK | 0o777;
    w.insert(bin, b"link", &link).unwrap();
    w.insert(root, b".hidden", &file(3, "2024-06-01T00:00:00Z"))
        .unwrap();
    w.insert(root, b"a b.txt", &file(3, "2024-06-01T00:00:00Z"))
        .unwrap();
    w.insert(root, b"bad\xFF.bin", &file(3, "2024-06-01T00:00:00Z"))
        .unwrap();
    w.publish()
}

fn run(g: &Arc<Generation>, q: &str, opts: SearchOptions, sort: Sort) -> Vec<String> {
    let macros = Macros::default();
    let ctx = CompileContext {
        options: opts,
        // Wednesday 2024-06-12 15:30 UTC.
        clock: Clock::at(1_718_206_200, TimeZone::UTC),
        macros: &macros,
        users: &NumericIds,
    };
    let compiled = compile_str(q, &ctx).unwrap_or_else(|e| panic!("{q}: {e}"));
    let cancel = AtomicBool::new(false);
    let sctx = SearchContext::basic(&cancel);
    let r = search(g, &compiled, sort, &sctx).unwrap();
    r.ids
        .iter()
        .map(|&id| String::from_utf8_lossy(&g.path(id)).into_owned())
        .collect()
}

fn q(g: &Arc<Generation>, query: &str) -> Vec<String> {
    let mut v = run(g, query, SearchOptions::default(), Sort::default());
    v.sort();
    v
}

fn s(v: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = v.iter().map(|x| (*x).to_owned()).collect();
    v.sort();
    v
}

#[test]
fn basic_name_search() {
    let g = fixture();
    assert_eq!(
        q(&g, "report"),
        s(&["/t/docs/Report.PDF", "/t/docs/report-old.pdf"])
    );
    assert_eq!(
        q(&g, "REPORT pdf"),
        s(&["/t/docs/Report.PDF", "/t/docs/report-old.pdf"])
    );
    assert_eq!(q(&g, "report !old"), s(&["/t/docs/Report.PDF"]));
    assert_eq!(
        q(&g, "song | notes"),
        s(&[
            "/t/docs/notes.txt",
            "/t/music/song.flac",
            "/t/music/song.mp3"
        ])
    );
    assert_eq!(q(&g, "\"a b\""), s(&["/t/a b.txt"]));
    assert_eq!(q(&g, "café"), s(&["/t/docs/Café.md"]));
    assert_eq!(q(&g, "CAFÉ"), s(&["/t/docs/Café.md"]));
    assert_eq!(q(&g, "cafe"), Vec::<String>::new());
    assert_eq!(q(&g, "nodiacritics:cafe"), s(&["/t/docs/Café.md"]));
    assert_eq!(q(&g, "\u{FF}"), Vec::<String>::new());
    assert_eq!(q(&g, "bad"), s(&["/t/bad\u{FFFD}.bin"]));
    assert_eq!(q(&g, "case:report"), s(&["/t/docs/report-old.pdf"]));
    assert_eq!(
        q(&g, "ww:song"),
        s(&["/t/music/song.flac", "/t/music/song.mp3"])
    );
    assert_eq!(q(&g, "ww:son"), Vec::<String>::new());
}

#[test]
fn wildcards_and_regex() {
    let g = fixture();
    assert_eq!(
        q(&g, "*.pdf"),
        s(&["/t/docs/Report.PDF", "/t/docs/report-old.pdf"])
    );
    assert_eq!(q(&g, "song.???"), s(&["/t/music/song.mp3"]));
    assert_eq!(
        q(&g, "nowfn:rep*t"),
        s(&["/t/docs/Report.PDF", "/t/docs/report-old.pdf"])
    );
    // `|` is the OR operator unless quoted, as in Everything.
    assert_eq!(
        q(&g, "regex:\"^s.*\\.(mp3|flac)$\""),
        s(&["/t/music/song.flac", "/t/music/song.mp3"])
    );
    assert_eq!(q(&g, "case:regex:^R"), s(&["/t/docs/Report.PDF"]));
}

#[test]
fn path_matching() {
    let g = fixture();
    assert_eq!(
        q(&g, "docs/"),
        s(&[
            "/t/docs/Café.md",
            "/t/docs/Report.PDF",
            "/t/docs/notes.txt",
            "/t/docs/report-old.pdf"
        ])
    );
    assert_eq!(
        q(&g, "/t/music/song"),
        s(&["/t/music/song.flac", "/t/music/song.mp3"])
    );
    assert_eq!(
        q(&g, "path:usic/so"),
        s(&["/t/music/song.flac", "/t/music/song.mp3"])
    );
    assert_eq!(
        q(&g, "path:t/m"),
        s(&["/t/music", "/t/music/song.flac", "/t/music/song.mp3"])
    );
    assert_eq!(q(&g, "path:*/bin/*"), s(&["/t/bin/link", "/t/bin/run.sh"]));
    assert_eq!(q(&g, "parent:/t/bin"), s(&["/t/bin/link", "/t/bin/run.sh"]));
    assert_eq!(
        q(&g, "infolder:/T/MUSIC"),
        s(&["/t/music/song.flac", "/t/music/song.mp3"])
    );
    assert_eq!(q(&g, "parent:/"), Vec::<String>::new());
}

#[test]
fn functions() {
    let g = fixture();
    assert_eq!(
        q(&g, "ext:pdf"),
        s(&["/t/docs/Report.PDF", "/t/docs/report-old.pdf"])
    );
    assert_eq!(
        q(&g, "ext:mp3;flac"),
        s(&["/t/music/song.flac", "/t/music/song.mp3"])
    );
    assert_eq!(q(&g, "case:ext:PDF"), s(&["/t/docs/Report.PDF"]));
    assert_eq!(q(&g, "ext: file:"), s(&["/t/.hidden", "/t/bin/link"]));
    assert_eq!(
        q(&g, "size:>1mb file:"),
        s(&[
            "/t/docs/report-old.pdf",
            "/t/music/song.flac",
            "/t/music/song.mp3"
        ])
    );
    // /t holds 39 MiB in total, /t/music 34 MiB.
    assert_eq!(
        q(&g, "size:huge"),
        s(&["/t", "/t/music", "/t/music/song.flac"])
    );
    assert_eq!(q(&g, "size:1kb"), s(&["/t/docs/notes.txt"]));
    assert_eq!(
        q(&g, "size:4mb..10mb file:"),
        s(&["/t/docs/report-old.pdf", "/t/music/song.mp3"])
    );
    assert_eq!(q(&g, "dm:today"), s(&["/t/docs/Report.PDF"]));
    assert_eq!(
        q(&g, "dm:thisweek file:"),
        s(&["/t/docs/Report.PDF", "/t/docs/notes.txt"])
    );
    assert_eq!(
        q(&g, "dm:2022"),
        s(&["/t/music/song.flac", "/t/music/song.mp3"])
    );
    assert_eq!(
        q(&g, "dm:<2023 file:"),
        s(&["/t/music/song.flac", "/t/music/song.mp3"])
    );
    assert_eq!(
        q(&g, "dc:unknown file: ext:pdf"),
        s(&["/t/docs/Report.PDF", "/t/docs/report-old.pdf"])
    );
    assert_eq!(
        q(&g, "audio:"),
        s(&["/t/music/song.flac", "/t/music/song.mp3"])
    );
    assert_eq!(q(&g, "audio:flac"), s(&["/t/music/song.flac"]));
    assert_eq!(
        q(&g, "doc:"),
        s(&[
            "/t/a b.txt",
            "/t/docs/Café.md",
            "/t/docs/Report.PDF",
            "/t/docs/notes.txt",
            "/t/docs/report-old.pdf"
        ])
    );
    assert_eq!(q(&g, "exe:"), s(&["/t/bin/run.sh"]));
    assert_eq!(
        q(&g, "folder:"),
        s(&[
            "/t",
            "/t/bin",
            "/t/docs",
            "/t/empty",
            "/t/music",
            "/t/private"
        ])
    );
    assert_eq!(q(&g, "folder:s"), s(&["/t/docs", "/t/music"]));
    assert_eq!(q(&g, "empty:"), s(&["/t/empty"]));
    assert_eq!(q(&g, "child:song"), s(&["/t/music"]));
    assert_eq!(q(&g, "childfolder:"), s(&["/t"]));
    assert_eq!(q(&g, "childcount:2 folder:"), s(&["/t/bin", "/t/music"]));
    assert_eq!(q(&g, "childfilecount:>3"), s(&["/t/docs"]));
    assert_eq!(
        q(&g, "startwith:rep"),
        s(&["/t/docs/Report.PDF", "/t/docs/report-old.pdf"])
    );
    assert_eq!(
        q(&g, "endwith:.pdf"),
        s(&["/t/docs/Report.PDF", "/t/docs/report-old.pdf"])
    );
    assert_eq!(q(&g, "len:4 file:"), s(&["/t/bin/link"]));
    assert_eq!(q(&g, "depth:1"), s(&["/t"]));
    assert_eq!(q(&g, "depth:3 ext:mp3"), s(&["/t/music/song.mp3"]));
    assert_eq!(q(&g, "root:"), s(&["/t"]));
    assert_eq!(q(&g, "symlink:"), s(&["/t/bin/link"]));
    assert_eq!(q(&g, "hidden:"), s(&["/t/.hidden"]));
    assert_eq!(q(&g, "attrib:L"), s(&["/t/bin/link"]));
    assert_eq!(q(&g, "attrib:H"), s(&["/t/.hidden"]));
    assert_eq!(q(&g, "perm:+x file:"), s(&["/t/bin/link", "/t/bin/run.sh"]));
    assert_eq!(q(&g, "perm:600"), s(&["/t/private/secret.key"]));
    assert_eq!(
        q(&g, "owner:1000"),
        s(&["/t/private", "/t/private/secret.key"])
    );
    assert_eq!(q(&g, "uid:>0 file:"), s(&["/t/private/secret.key"]));
    assert_eq!(q(&g, "dupe: file:"), s(&[]));
    assert_eq!(
        q(&g, "namepartdupe:"),
        s(&["/t/music/song.flac", "/t/music/song.mp3"])
    );
    assert_eq!(
        q(&g, "sizedupe: file: size:3"),
        s(&["/t/.hidden", "/t/a b.txt", "/t/bad\u{FFFD}.bin"])
    );
    assert_eq!(q(&g, "fstype:ext4 root:"), s(&["/t"]));
    assert_eq!(q(&g, "fstype:btrfs"), s(&[]));
    assert_eq!(q(&g, "volume:fixture root:"), s(&["/t"]));
    assert_eq!(q(&g, "type:folder empty"), s(&["/t/empty"]));
    assert_eq!(q(&g, "runcount:0 song.mp3"), s(&["/t/music/song.mp3"]));
}

#[test]
fn sorting() {
    let g = fixture();
    let by = |field, descending| {
        run(
            &g,
            "file: !bad",
            SearchOptions::default(),
            Sort { field, descending },
        )
    };
    let names = |v: Vec<String>| {
        v.into_iter()
            .map(|p| p.rsplit('/').next().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(by(SortField::Name, false)),
        [
            ".hidden",
            "a b.txt",
            "Café.md",
            "link",
            "notes.txt",
            "report-old.pdf",
            "Report.PDF",
            "run.sh",
            "secret.key",
            "song.flac",
            "song.mp3"
        ]
    );
    let size = names(by(SortField::Size, true));
    assert_eq!(&size[..3], ["song.flac", "report-old.pdf", "song.mp3"]);
    let ext = names(by(SortField::Extension, false));
    assert_eq!(ext[0], ".hidden");
    assert_eq!(ext.last().unwrap(), "notes.txt");
    let path = by(SortField::Path, false);
    assert_eq!(path[0], "/t/.hidden");
    assert_eq!(path.last().unwrap(), "/t/private/secret.key");
    let dm = names(by(SortField::Modified, true));
    assert_eq!(dm[0], "Report.PDF");
}

#[test]
fn highlights_and_options() {
    let g = fixture();
    let macros = Macros::default();
    let ctx = CompileContext {
        options: SearchOptions::default(),
        clock: Clock::at(1_718_206_200, TimeZone::UTC),
        macros: &macros,
        users: &NumericIds,
    };
    let c = compile_str("rep pdf", &ctx).unwrap();
    let cancel = AtomicBool::new(false);
    let r = search(&g, &c, Sort::default(), &SearchContext::basic(&cancel)).unwrap();
    let id = *r.ids.iter().find(|&&i| g.name(i) == b"Report.PDF").unwrap();
    assert_eq!(highlights(&g, &c, id).name, vec![(0, 3), (7, 10)]);
    // Global options.
    let o = SearchOptions {
        match_case: true,
        ..SearchOptions::default()
    };
    assert_eq!(
        run(&g, "Report", o, Sort::default()),
        vec!["/t/docs/Report.PDF"]
    );
    let o = SearchOptions {
        match_path: true,
        ..SearchOptions::default()
    };
    assert_eq!(run(&g, "music", o, Sort::default()).len(), 3);
    let o = SearchOptions {
        regex: true,
        ..SearchOptions::default()
    };
    assert_eq!(
        run(&g, "^song\\.mp3$", o, Sort::default()),
        vec!["/t/music/song.mp3"]
    );
}

#[test]
fn cancellation_and_visibility() {
    let g = fixture();
    let macros = Macros::default();
    let ctx = CompileContext {
        options: SearchOptions::default(),
        clock: Clock::system(),
        macros: &macros,
        users: &NumericIds,
    };
    let c = compile_str("", &ctx).unwrap();
    let cancel = AtomicBool::new(true);
    assert_eq!(
        search(&g, &c, Sort::default(), &SearchContext::basic(&cancel)).unwrap_err(),
        SearchError::Cancelled
    );
    // Visibility: only entries whose bit is set are returned.
    let cancel = AtomicBool::new(false);
    let mut vis = Bitmap::with_capacity(g.capacity());
    let some = g.iter().find(|&id| g.name(id) == b"notes.txt").unwrap();
    vis.set(some.0);
    let mut sc = SearchContext::basic(&cancel);
    sc.visible = Some(&vis);
    let r = search(&g, &c, Sort::default(), &sc).unwrap();
    assert_eq!(r.ids, vec![some]);
}
