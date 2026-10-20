# Fossick search syntax

Fossick follows the search syntax of voidtools Everything 1.4, plus the 1.5 additions that make
sense on Linux and a few Linux-specific functions. Every rule on this page is exercised by the
compatibility corpus in [`tests/corpus/`](../tests/corpus), and each corpus case names its source.

> The official voidtools pages could not be fetched from the development sandbox, so the syntax was
> assembled from search-engine copies of the “Searching” and “Search Functions” help pages, forum
> clarifications, and known Everything behaviour. Where Everything’s behaviour is ambiguous,
> Fossick’s choice is stated below. Please report differences from real Everything.

## Basics

| Input | Meaning |
|---|---|
| `foo` | names containing `foo` (case-insensitive) |
| `foo bar` | AND: names containing both |
| <code>foo&#124;bar</code> | OR |
| `!foo` | NOT |
| `<foo bar>` | grouping |
| `"foo bar"` | exact phrase (quotes protect spaces and <code>&#124; &lt; &gt; !</code>) |

- **OR binds tighter than AND**: <code>a b&#124;c</code> means `a AND (b OR c)`.
- **Quotes can appear anywhere in a term**: `foo" "bar` is the phrase `foo bar`.
- **Unbalanced brackets are forgiven**: a missing `>` is added at the end, and a stray `>` is
  ignored.
- **An empty search matches everything.**
- **A term containing `/` matches against the full path** instead of the name (Everything does this
  for `\`).

## Matching

- **Default:** case-insensitive substring on the name, using Unicode *simple* case folding (`Ä` =
  `ä`, `Σ` = `σ` = `ς`, `K` [Kelvin sign] = `k`). Simple folding maps one character to one
  character, so `ß` does **not** match `ss`.
- **Normalization:** names are compared in Unicode NFC, so a decomposed `e` + U+0301 matches `é`.
- **Diacritics are matched by default.** Turn on the diacritics-insensitive option or use
  `nodiacritics:` to make `cafe` match `café`. This strips combining marks and maps a few letters
  that don’t decompose (`ø ł đ ħ ŧ ı`) to their base letter.
- **Names that aren’t valid UTF-8** are searchable. Invalid bytes match only themselves and
  display as `\xNN`.

### Wildcards

- `*` matches any run of characters (including none); `?` matches exactly one character.
- A term with a wildcard must match the **whole name** (`*.pdf`, `IMG_000?.jpg`).
  `nowfn:` / `nowholefilename:` turns that off, so `nowfn:rep*rt` finds `my-report.pdf`.
- `nowildcards:` makes `*` and `?` literal.

### Global options

The GUI shortcuts are the same as in Everything. Each option can be overridden per term with a
modifier.

| Option | Shortcut | CLI |
|---|---|---|
| Match Case | Ctrl+I | `--case` |
| Match Whole Word | Ctrl+B | `-w`, `--whole-word` |
| Match Path | Ctrl+U | `-p`, `--path` |
| Regex | Ctrl+R | `-r`, `--regex` |
| Match Diacritics (on by default) | — | `--ignore-diacritics` turns it off |

## Modifiers

Modifiers prefix a term, a function or a group, as in `case:Foo`, `case:ext:JPG` or
<code>case:&lt;A&#124;B&gt;</code>. Several can be combined (`case:regex:^IMG`).

| Modifier | Effect |
|---|---|
| `case:` / `nocase:` | match case / ignore case |
| `ww:` `wholeword:` / `noww:` `nowholeword:` | whole words only. Word characters are letters, digits and `_`, so `IMG_0001` contains no whole word `img`. |
| `path:` / `nopath:` | match the full path / the name only |
| `regex:` / `noregex:` | the term is a regular expression (Rust `regex` syntax, linear time). <code>&#124;</code> must be inside quotes: <code>regex:"^a(b&#124;c)$"</code>. |
| `diacritics:` / `nodiacritics:` | match diacritics exactly / ignore them |
| `wfn:` `wholefilename:` / `nowfn:` `nowholefilename:` | the term must equal the whole name / wildcards match anywhere in the name |
| `wildcards:` / `nowildcards:` | `*` and `?` are wildcards / literal characters |
| `ascii:` / `utf8:` `noascii:` | fold ASCII letters only (no Unicode folding or normalization) / the default |
| `file:` `files:` | files only. On its own, `file:` matches all files. |
| `folder:` `folders:` | folders only. On its own, `folder:` matches all folders. |
| `nofileonly:` `nofolderonly:` | cancel `file:` / `folder:` |

## Functions

Functions take the form `name:argument`. Names are case-insensitive. Arguments containing spaces
or <code>&#124; &lt; &gt;</code> must be quoted: `parent:"/home/me/My Files"`.

### Comparisons and ranges

Numeric, size and date functions accept these argument forms. Each value denotes an interval:
`1mb` is exactly 1 MiB, `2024` is the whole year, `today` is the whole day.

| Form | Meaning |
|---|---|
| `v` or `=v` | inside `v` |
| `<v` | before `v` starts |
| `<=v` | up to the end of `v` |
| `>v` | after `v` ends |
| `>=v` | from the start of `v` |
| `a..b` | from the start of `a` to the end of `b`; either side may be left out |

### Size

`size:` compares the file size, or the folder size for folders. A folder’s size is the sum of all
files below it and stops at mount points.

- **Units:** `b`, `kb`/`k`/`kib`, `mb`/`m`/`mib`, `gb`, `tb`, `pb`, all 1024-based. Fractions are
  allowed (`size:1.5mb`).
- **Constants**, as in Everything:

  | Constant | Range |
  |---|---|
  | `empty` | 0 |
  | `tiny` | 0 < size ≤ 10 KB |
  | `small` | 10 KB < size ≤ 100 KB |
  | `medium` | 100 KB < size ≤ 1 MB |
  | `large` | 1 MB < size ≤ 16 MB |
  | `huge` | 16 MB < size ≤ 128 MB |
  | `gigantic` | over 128 MB |

- **Examples:** `size:>1gb`, `size:10mb..100mb`, `size:<=5kb`, `size:huge`, `size:empty`.

### Dates

| Function | Field |
|---|---|
| `dm:` `datemodified:` | modification time (mtime) |
| `dc:` `datecreated:` | birth time (statx btime; `unknown` where the filesystem doesn’t record it) |
| `da:` `dateaccessed:` | access time. Only when access-time indexing is enabled (`fossick --atime`); refreshed by rescans, not live. |
| `dch:` `datechanged:` | status change time (ctime). A Linux addition. |
| `dr:` `daterun:` | last time the file was opened through Fossick |

**Absolute dates.** `2024`, `2024-05`, `2024-05-17`, `2024-05-17T10`, `2024-05-17T10:30`,
`2024-05-17T10:30:15`. `/` is accepted instead of `-`, and a quoted space instead of `T`. Local
formats (`17/05/2024`) are not accepted, because they are ambiguous.

**Constants.** These are evaluated in the searcher’s local time zone. Weeks start on Monday.

| Constant | Meaning |
|---|---|
| `today`, `yesterday`, `tomorrow`, `now` | |
| `thisweek` `thismonth` `thisyear` (also `current…`) | the current calendar period |
| `lastweek` `lastmonth` `lastyear` (also `prev…`, `previous…`) | the previous calendar period |
| `prev2months`, `last3weeks`, … | the N previous calendar periods, not including the current one |
| `nextweek`, `nextmonth`, `next2years`, … (also `coming…`) | the following calendar periods |
| `pastweek`, `past3days`, `past2hours`, … | rolling window ending now (`pastweek` = `past7days`) |
| `2days`, `3h`, `10m`, … (Everything 1.5 form) | rolling window ending now. Units: `y`, `w`, `d`, `h`, `m` (minutes), `s`, or spelled out. |
| `last2hours`, `last30minutes` | for hours, minutes and seconds, `last` is rolling as well |
| `january` … `december`, `jan` … `dec` | that month of the current year |
| `monday` … `sunday`, `mon` … `sun` | the most recent such day, today included |
| `mtd`, `qtd`, `ytd` | month, quarter or year to date |
| `unknown` | the time isn’t known |

Examples: `dm:today`, `dm:lastweek`, `dm:2024`, `dm:2024-01..2024-03`, `dc:unknown`, `dm:>=2024-06-10`.

### Name and path

| Function | Meaning |
|---|---|
| `ext:jpg;png` | files whose last extension is in the list. `ext:` with no argument matches files without an extension. A leading dot (`.bashrc`) is not an extension. |
| `startwith:` / `endwith:` | the name starts / ends with the text |
| `len:` | name length in characters |
| `parent:` `infolder:` `nosubfolders:` | entries directly inside this folder (full path; case-insensitive unless `case:`) |
| `depth:` `parents:` | number of path components (`/` = 0, `/home` = 1) |
| `root:` | volume roots (entries without a parent folder) |
| `type:` | substring of the type: the MIME type for files (shared-mime-info), `inode/directory folder` for folders |

### Folders and children

| Function | Meaning |
|---|---|
| `empty:` | folders without any children |
| `child:text` / `childfile:text` / `childfolder:text` | folders with a child (file / folder) matching the text. Without an argument: folders with at least one child (file / folder). |
| `childcount:` `childfilecount:` `childfoldercount:` | number of direct children |

### Duplicates

| Function | Meaning |
|---|---|
| `dupe:` | entries whose name occurs more than once in the whole index (visible entries only) |
| `dupe:size;name` | duplicates by several properties: `name`, `namepart` (stem), `ext`, `size`, `dm`, `dc` |
| `sizedupe:` `namepartdupe:` `dmdupe:` `dcdupe:` `extdupe:` | shorthands |

### Attributes and permissions (Linux)

`attrib:` / `attributes:` letters must **all** apply. Everything’s Windows attributes map to Linux
as follows:

| Letter | Linux meaning |
|---|---|
| `R` | no write permission bits |
| `H` | dot name (hidden) |
| `S` | special file (device, FIFO, socket) |
| `D` | directory |
| `A` | regular file |
| `N` | regular, not hidden, writable, no special attributes |
| `L` | symbolic link |
| `C` | compressed (statx) |
| `E` | encrypted (statx) |
| `P` | sparse (allocated blocks < size) |
| `V` | fs-verity |
| `O` | on an offline volume |
| `T` `I` `X` `U` `M` `B` | no Linux equivalent; never match |

| Function | Meaning |
|---|---|
| `perm:755` | exact permission bits (octal) |
| `perm:rwxr-xr-x` | exact permission bits (`ls` style) |
| `perm:u+x` | the given bits are all set |
| `perm:+x` | any execute bit set |
| `perm:o-w` | the given bits are not set |
| `owner:name` / `owner:1000` | owner, by name (NSS) or uid |
| `group:name` / `group:1000` | group, by name or gid |
| `uid:` / `gid:` | numeric comparisons |
| `symlink:` | symbolic links |
| `hidden:` | dot names |
| `inode:` `frn:` | inode number |

### Volumes

| Function | Meaning |
|---|---|
| `fstype:ext4;btrfs` | entries on volumes of these filesystem types |
| `volume:text` | volume label, identity (UUID) or mount path contains the text |
| `fsi:` | volume index |

### Contents and history

| Function | Meaning |
|---|---|
| `content:text` | file contents contain the text (ASCII case-insensitive unless `case:`; `regex:content:"…"` for regex). Read on demand and cancellable, with a per-file read limit. Very slow on large result sets, so combine it with other filters. |
| `runcount:` | how many times the file was opened through Fossick |
| `dr:` | when it was last opened through Fossick (see Dates) |

### Not supported yet

These return a clear “not supported yet” error instead of matching anything:

- `width:` `height:` `dimensions:` `orientation:` `bitdepth:`
- `album:` `artist:` `genre:` `title:` `track:` `comment:` `year:` `length:`
- `tag:`
- `rc:`
- `filelist:` `filelistfilename:`
- `shell:`

See [`TODO.md`](TODO.md).

## Macros

Built-in macros filter by extension (macro lists are extended for Linux):

| Macro | Matches |
|---|---|
| `audio:` | aac, ac3, aif(f/c), amr, ape, au, cda, dts, fla(c), it, m1a, m2a, m3u, m4a, m4b, mid(i), mka, mod, mp2, mp3, mpa, ogg, opus, ra, rmi, snd, spc, umx, voc, wav, wma, wv, xm |
| `zip:` | 7z, ace, arj, br, bz2, cab, cpio, deb, gz(ip), jar, lz, lz4, lzma, lzo, r00–r29, rar, rpm, tar, tbz(2), tgz, txz, tzst, war, xz, z, zip, zst |
| `doc:` | office and text formats: pdf, doc(x/m), odt, ods, odp, xls(x), ppt(x), rtf, txt, md, rst, tex, epub, html, xml, csv, ini, c/cpp/h/java/lua sources … |
| `pic:` | ani, avif, bmp, cr2, dng, gif, heic/heif, ico, jfif, jp(e)g, jxl, nef, orf, pcx, png, psd, raw, svg, tga, tif(f), webp, wmf, xcf |
| `video:` | 3gp(2), amv, asf, avi, bik, divx, dv, f4v, flv, h264, m2ts/mts, m4v, mkv, mov, mp4, mpeg/mpg, mxf, ogv, qt, rm(vb), swf, ts, vob, webm, wmv … |
| `exe:` | regular files with any execute bit, plus appimage, bat, cmd, com, exe, msi, msp, run, scr, sh |

A macro with an argument also matches the text: `audio:abba` finds audio files whose names
contain `abba`.

User macros map a name to a query. In the CLI they are defined with
`--macro 'media=audio:|video:'`; GUI settings follow in Phase 3. After that definition, `media:`
works like a built-in macro. Macros may use other macros, nested up to 8 levels deep.

## Sorting

Sortable by name, path, size, extension, type, date modified, created, accessed, changed, run
count and date run, ascending or descending.

- Name, path, size and date modified are kept presorted, so re-sorting millions of results is
  instant. The other columns are sorted on request.
- Ties break by name. When sorting by name, equal names break by path.
- Names sort case-insensitively by Unicode code point (not locale-aware).
- Path order compares one component at a time, so a folder’s contents stay together.
