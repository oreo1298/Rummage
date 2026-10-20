//! Compiles a query tree into an executable predicate tree.

use std::collections::{HashMap, HashSet};

use super::ast::{KindOnly, Mods, Node};
use super::matcher::{TextMatch, TextMode, wildcard_to_regex};
use super::parse::{ParseOptions, parse};
use super::value::{Clock, Range, parse_date, parse_number, parse_range, parse_size};
use crate::index::TimeField;

/// Global search options (the GUI toggles; per-term modifiers override them).
#[derive(Clone, Copy, Debug)]
pub struct SearchOptions {
    /// Match Case (Ctrl+I).
    pub match_case: bool,
    /// Match Whole Word (Ctrl+B).
    pub whole_word: bool,
    /// Match Path (Ctrl+U).
    pub match_path: bool,
    /// Regex (Ctrl+R).
    pub regex: bool,
    /// Match diacritics exactly. `false` = diacritics-insensitive.
    pub match_diacritics: bool,
    /// Terms containing wildcards must match the whole name (Everything’s default).
    pub wildcards_whole_name: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            match_case: false,
            whole_word: false,
            match_path: false,
            regex: false,
            match_diacritics: true,
            wildcards_whole_name: true,
        }
    }
}

/// Query compilation errors.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    #[error("{0}: is not supported yet")]
    NotSupported(String),
    #[error("{func}: {message}")]
    BadArgument { func: String, message: String },
    #[error("invalid regular expression: {0}")]
    Regex(String),
    #[error("unknown user or group {0:?}")]
    UnknownId(String),
    #[error("macro {0:?} expands too deeply")]
    MacroDepth(String),
}

/// Resolves user and group names (`owner:alice`).
pub trait UserDb: Sync {
    fn uid_of(&self, name: &str) -> Option<u32>;
    fn gid_of(&self, name: &str) -> Option<u32>;
}

/// A [`UserDb`] that only understands numeric ids.
#[derive(Clone, Copy, Debug, Default)]
pub struct NumericIds;

impl UserDb for NumericIds {
    fn uid_of(&self, _: &str) -> Option<u32> {
        None
    }
    fn gid_of(&self, _: &str) -> Option<u32> {
        None
    }
}

/// Built-in and user-defined macros.
#[derive(Clone, Debug, Default)]
pub struct Macros {
    /// User macros: name → query text.
    pub user: HashMap<String, String>,
}

pub(crate) const AUDIO: &str = "aac;ac3;aif;aifc;aiff;amr;ape;au;cda;dts;fla;flac;it;m1a;m2a;m3u;m4a;m4b;mid;midi;mka;mod;mp2;mp3;mpa;ogg;opus;ra;rmi;snd;spc;umx;voc;wav;wma;wv;xm";
pub(crate) const ZIP: &str = "7z;ace;arj;br;bz2;cab;cpio;deb;gz;gzip;jar;lz;lz4;lzma;lzo;r00;r01;r02;r03;r04;r05;r06;r07;r08;r09;r10;r11;r12;r13;r14;r15;r16;r17;r18;r19;r20;r21;r22;r23;r24;r25;r26;r27;r28;r29;rar;rpm;tar;tbz;tbz2;tgz;txz;tzst;war;xz;z;zip;zst";
pub(crate) const DOC: &str = "c;chm;cpp;csv;cxx;doc;docm;docx;dot;dotm;dotx;epub;h;hpp;htm;html;hxx;ini;java;lua;md;mht;mhtml;odp;ods;odt;pdf;potm;potx;ppam;pps;ppsm;ppsx;ppt;pptm;pptx;rst;rtf;sldm;sldx;tex;thmx;txt;vsd;wpd;wps;wri;xlam;xls;xlsb;xlsm;xlsx;xltm;xltx;xml";
pub(crate) const EXE: &str = "appimage;bat;cmd;com;exe;msi;msp;run;scr;sh";
pub(crate) const PIC: &str = "ani;avif;bmp;cr2;dng;gif;heic;heif;ico;jfif;jpe;jpeg;jpg;jxl;nef;orf;pcx;png;psd;raw;svg;tga;tif;tiff;webp;wmf;xcf";
pub(crate) const VIDEO: &str = "3g2;3gp;3gp2;3gpp;amv;asf;avi;bik;divx;drc;dv;f4v;flv;h264;m1v;m2t;m2ts;m2v;m4v;mkv;mov;mp4;mp4v;mpe;mpeg;mpg;mpv;mts;mxf;ogm;ogv;qt;rm;rmvb;swf;ts;vob;webm;wmv";

/// Which children `child*:` functions look at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildKind {
    Any,
    Files,
    Folders,
}

/// Properties `dupe:` groups by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DupeProp {
    Name,
    NamePart,
    Ext,
    Size,
    Modified,
    Created,
}

/// Permission tests for `perm:`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermTest {
    /// Exact permission bits (`perm:755`).
    Exact(u32),
    /// All of these bits set (`perm:u+x`).
    AllSet(u32),
    /// Any of these bits set (`perm:+x` = any execute bit).
    AnySet(u32),
    /// None of these bits set (`perm:o-w`).
    NoneSet(u32),
}

/// Attribute letters for `attrib:` (Linux interpretation, see docs/SYNTAX.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AttribSet {
    pub readonly: bool,
    pub hidden: bool,
    pub system: bool,
    pub directory: bool,
    pub archive: bool,
    pub normal: bool,
    pub sparse: bool,
    pub reparse: bool,
    pub compressed: bool,
    pub offline: bool,
    pub encrypted: bool,
    pub integrity: bool,
    pub never: bool,
}

/// An executable predicate.
#[derive(Clone, Debug)]
pub enum Pred {
    True,
    False,
    And(Vec<Pred>),
    Or(Vec<Pred>),
    Not(Box<Pred>),
    Kind(KindOnly),
    Name(Box<TextMatch>),
    Path(Box<TextMatch>),
    /// Extensions (normalized; an empty string matches names without extension). Files only.
    Ext {
        exts: HashSet<Vec<u8>>,
        case: bool,
    },
    Size(Range),
    Time(TimeField, Range),
    TimeUnknown(TimeField),
    Len(Range),
    Depth(Range),
    ChildCount(ChildKind, Range),
    Inode(Range),
    VolumeIndex(Range),
    Uid(Range),
    Gid(Range),
    /// Full path of the parent folder equals this (normalized per `case`).
    Parent {
        path: Vec<u8>,
        case: bool,
    },
    /// Folders with at least one child (of the given kind) matching the predicate.
    Child(ChildKind, Box<Pred>),
    /// Folders without children.
    Empty,
    /// Volume roots.
    Root,
    Symlink,
    Hidden,
    Attrib(AttribSet),
    Perm(PermTest),
    FsType(Vec<String>),
    Volume(Box<TextMatch>),
    Content(Box<TextMatch>),
    Dupe(Vec<DupeProp>),
    RunCount(Range),
    DateRun(Range),
    Type(Box<TextMatch>),
}

impl Pred {
    /// Rough evaluation cost, used to order AND/OR children (cheap first).
    #[must_use]
    pub fn cost(&self) -> u32 {
        match self {
            Self::True | Self::False => 0,
            Self::Kind(_) | Self::Symlink | Self::Hidden | Self::Root | Self::Empty => 1,
            Self::Size(_)
            | Self::Time(..)
            | Self::TimeUnknown(_)
            | Self::Inode(_)
            | Self::VolumeIndex(_)
            | Self::Uid(_)
            | Self::Gid(_)
            | Self::Perm(_)
            | Self::Attrib(_)
            | Self::ChildCount(..)
            | Self::RunCount(_)
            | Self::DateRun(_)
            | Self::FsType(_) => 2,
            Self::Ext { .. } | Self::Len(_) | Self::Volume(_) => 4,
            Self::Name(m) => {
                if m.is_regex() {
                    20
                } else {
                    8
                }
            }
            Self::Type(_) => 12,
            Self::Depth(_) | Self::Parent { .. } => 16,
            Self::Path(_) => 40,
            Self::Child(..) | Self::Dupe(_) => 60,
            Self::Content(_) => 1000,
            Self::Not(p) => p.cost(),
            Self::And(c) | Self::Or(c) => c.iter().map(Self::cost).sum(),
        }
    }

    /// Text matchers that should be highlighted in names / paths (positive context only).
    pub fn highlight_matchers<'a>(
        &'a self,
        names: &mut Vec<&'a TextMatch>,
        paths: &mut Vec<&'a TextMatch>,
    ) {
        match self {
            Self::Name(m) => names.push(m),
            Self::Path(m) => paths.push(m),
            Self::And(c) | Self::Or(c) => {
                for p in c {
                    p.highlight_matchers(names, paths);
                }
            }
            _ => {}
        }
    }

    /// True if this predicate needs file contents.
    #[must_use]
    pub fn needs_content(&self) -> bool {
        match self {
            Self::Content(_) => true,
            Self::Not(p) | Self::Child(_, p) => p.needs_content(),
            Self::And(c) | Self::Or(c) => c.iter().any(Self::needs_content),
            _ => false,
        }
    }
}

/// Everything needed to compile a query.
pub struct CompileContext<'a> {
    pub options: SearchOptions,
    pub clock: Clock,
    pub macros: &'a Macros,
    pub users: &'a dyn UserDb,
}

impl std::fmt::Debug for CompileContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompileContext")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

/// A compiled query.
#[derive(Clone, Debug)]
pub struct Compiled {
    pub source: String,
    pub tree: Node,
    pub pred: Pred,
}

/// Parses and compiles a query string.
pub fn compile_str(query: &str, ctx: &CompileContext<'_>) -> Result<Compiled, QueryError> {
    let opts = ParseOptions {
        macros: ctx
            .macros
            .user
            .keys()
            .map(|k| k.to_ascii_lowercase())
            .collect(),
    };
    let tree = parse(query, &opts);
    let pred = compile(&tree, ctx, 0)?;
    Ok(Compiled {
        source: query.to_owned(),
        tree,
        pred: simplify(pred),
    })
}

const MAX_MACRO_DEPTH: usize = 8;

/// Compiles a tree.
pub fn compile(node: &Node, ctx: &CompileContext<'_>, depth: usize) -> Result<Pred, QueryError> {
    Ok(match node {
        Node::And(c) => Pred::And(
            c.iter()
                .map(|n| compile(n, ctx, depth))
                .collect::<Result<_, _>>()?,
        ),
        Node::Or(c) => Pred::Or(
            c.iter()
                .map(|n| compile(n, ctx, depth))
                .collect::<Result<_, _>>()?,
        ),
        Node::Not(c) => Pred::Not(Box::new(compile(c, ctx, depth)?)),
        Node::Text { text, mods } => with_kind(text_pred(text, mods, ctx)?, mods),
        Node::Func { name, arg, mods } => with_kind(func_pred(name, arg, mods, ctx, depth)?, mods),
    })
}

fn with_kind(p: Pred, mods: &Mods) -> Pred {
    match mods.kind {
        Some(k @ (KindOnly::Files | KindOnly::Folders)) => Pred::And(vec![Pred::Kind(k), p]),
        _ => p,
    }
}

fn case_of(mods: &Mods, ctx: &CompileContext<'_>) -> bool {
    mods.case.unwrap_or(ctx.options.match_case)
}

fn diacritics_of(mods: &Mods, ctx: &CompileContext<'_>) -> bool {
    mods.diacritics.unwrap_or(ctx.options.match_diacritics)
}

/// Builds the text matcher for a plain term.
fn text_matcher(
    text: &str,
    mods: &Mods,
    ctx: &CompileContext<'_>,
) -> Result<TextMatch, QueryError> {
    let case = case_of(mods, ctx);
    let diacritics = diacritics_of(mods, ctx);
    let ascii = mods.ascii.unwrap_or(false);
    if mods.regex.unwrap_or(ctx.options.regex) {
        return TextMatch::regex(text, case, diacritics).map_err(QueryError::Regex);
    }
    let wildcards = mods.wildcards.unwrap_or(true) && text.contains(['*', '?']);
    let whole_name = mods
        .whole_filename
        .unwrap_or(ctx.options.wildcards_whole_name);
    if wildcards {
        let anchored = mods.whole_filename.unwrap_or(whole_name);
        if !text.contains('?') {
            return Ok(TextMatch::glob(text, anchored, case, diacritics, ascii));
        }
        return TextMatch::regex(&wildcard_to_regex(text, anchored), case, diacritics)
            .map_err(QueryError::Regex);
    }
    let mode = if mods.whole_filename == Some(true) {
        TextMode::Exact
    } else if mods.whole_word.unwrap_or(ctx.options.whole_word) {
        TextMode::WholeWord
    } else {
        TextMode::Substring
    };
    Ok(TextMatch::literal(text, mode, case, diacritics, ascii))
}

fn text_pred(text: &str, mods: &Mods, ctx: &CompileContext<'_>) -> Result<Pred, QueryError> {
    if text.is_empty() {
        return Ok(Pred::True);
    }
    let m = text_matcher(text, mods, ctx)?;
    let path = mods
        .path
        .unwrap_or(ctx.options.match_path || text.contains('/'));
    Ok(if path {
        Pred::Path(Box::new(m))
    } else {
        Pred::Name(Box::new(m))
    })
}

fn bad(func: &str, message: impl Into<String>) -> QueryError {
    QueryError::BadArgument {
        func: func.to_owned(),
        message: message.into(),
    }
}

fn range(
    func: &str,
    arg: &str,
    value: &dyn Fn(&str) -> Result<super::value::Interval, String>,
) -> Result<Range, QueryError> {
    if arg.trim().is_empty() {
        return Err(bad(func, "expects a value"));
    }
    parse_range(arg, value).map_err(|m| bad(func, m))
}

fn no_arg(func: &str, arg: &str, p: Pred) -> Result<Pred, QueryError> {
    if arg.trim().is_empty() {
        Ok(p)
    } else {
        Err(bad(func, "takes no argument"))
    }
}

fn ext_set(list: &str, case: bool) -> HashSet<Vec<u8>> {
    list.split(';')
        .map(|e| {
            let e = e.trim().trim_start_matches('.');
            let mut out = Vec::new();
            super::matcher::normalize_into(e.as_bytes(), case, true, false, &mut out);
            out
        })
        .collect()
}

fn macro_pred(
    exts: &str,
    arg: &str,
    mods: &Mods,
    ctx: &CompileContext<'_>,
    extra: Option<Pred>,
) -> Result<Pred, QueryError> {
    let mut parts = vec![Pred::Ext {
        exts: ext_set(exts, false),
        case: false,
    }];
    if let Some(e) = extra {
        parts = vec![Pred::Or(vec![parts.remove(0), e])];
        parts.insert(0, Pred::Kind(KindOnly::Files));
    }
    if !arg.is_empty() {
        parts.push(text_pred(
            arg,
            &Mods {
                kind: None,
                ..*mods
            },
            ctx,
        )?);
    }
    Ok(Pred::And(parts))
}

fn ids(func: &str, arg: &str, resolve: &dyn Fn(&str) -> Option<u32>) -> Result<Range, QueryError> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Err(bad(func, "expects a name or number"));
    }
    if arg.starts_with(['<', '>', '=']) || arg.contains("..") || arg.parse::<u32>().is_ok() {
        return range(func, arg, &parse_number);
    }
    match resolve(arg) {
        Some(id) => Ok(Range {
            lo: Some(i128::from(id)),
            hi: Some(i128::from(id) + 1),
        }),
        None => Err(QueryError::UnknownId(arg.to_owned())),
    }
}

fn parse_perm(arg: &str) -> Result<PermTest, String> {
    let arg = arg.trim();
    if !arg.is_empty() && arg.len() <= 4 && arg.chars().all(|c| ('0'..='7').contains(&c)) {
        return u32::from_str_radix(arg, 8)
            .map(PermTest::Exact)
            .map_err(|e| e.to_string());
    }
    if arg.len() == 9 && arg.chars().all(|c| "rwxsStT-".contains(c)) {
        // ls-style "rwxr-xr-x".
        let mut bits = 0;
        for (i, c) in arg.chars().enumerate() {
            if c != '-' {
                bits |= 1 << (8 - i);
            }
        }
        return Ok(PermTest::Exact(bits));
    }
    let op_pos = arg.find(['+', '-']).ok_or_else(|| {
        format!("expected octal (755), ls-style (rwxr-xr-x) or [ugoa][+-][rwxst], got {arg:?}")
    })?;
    let (who, rest) = arg.split_at(op_pos);
    let (op, what) = rest.split_at(1);
    let mut who_mask = 0u32;
    let explicit_who = !who.is_empty();
    for c in who.chars() {
        who_mask |= match c {
            'u' => 0o700,
            'g' => 0o070,
            'o' => 0o007,
            'a' => 0o777,
            _ => return Err(format!("bad permission class {c:?}")),
        };
    }
    if !explicit_who {
        who_mask = 0o777;
    }
    let mut bits = 0u32;
    for c in what.chars() {
        bits |= match c {
            'r' => 0o444 & who_mask,
            'w' => 0o222 & who_mask,
            'x' => 0o111 & who_mask,
            's' => 0o6000,
            't' => 0o1000,
            _ => return Err(format!("bad permission {c:?}")),
        };
    }
    Ok(match (op, explicit_who) {
        ("+", true) => PermTest::AllSet(bits),
        ("+", false) => PermTest::AnySet(bits),
        _ => PermTest::NoneSet(bits),
    })
}

fn parse_attrib(arg: &str) -> Result<AttribSet, String> {
    let mut a = AttribSet::default();
    for c in arg.trim().chars() {
        match c.to_ascii_uppercase() {
            'R' => a.readonly = true,
            'H' => a.hidden = true,
            'S' => a.system = true,
            'D' => a.directory = true,
            'A' => a.archive = true,
            'N' => a.normal = true,
            'P' => a.sparse = true,
            'L' => a.reparse = true,
            'C' => a.compressed = true,
            'O' => a.offline = true,
            'E' => a.encrypted = true,
            'V' => a.integrity = true,
            'T' | 'I' | 'X' | 'U' | 'M' | 'B' => a.never = true,
            other => return Err(format!("unknown attribute letter {other:?}")),
        }
    }
    Ok(a)
}

fn func_pred(
    name: &str,
    arg: &str,
    mods: &Mods,
    ctx: &CompileContext<'_>,
    depth: usize,
) -> Result<Pred, QueryError> {
    let case = case_of(mods, ctx);
    let diacritics = diacritics_of(mods, ctx);
    let date = |v: &str| parse_date(v, &ctx.clock);
    let time = |field: TimeField| -> Result<Pred, QueryError> {
        if arg.trim().eq_ignore_ascii_case("unknown") {
            return Ok(Pred::TimeUnknown(field));
        }
        Ok(Pred::Time(field, range(name, arg, &date)?))
    };
    let literal = |mode: TextMode| {
        Box::new(TextMatch::literal(
            arg,
            mode,
            case,
            diacritics,
            mods.ascii.unwrap_or(false),
        ))
    };
    let child_pred = |kind: ChildKind| -> Result<Pred, QueryError> {
        let inner = if arg.is_empty() {
            Pred::True
        } else {
            text_pred(
                arg,
                &Mods {
                    kind: None,
                    path: Some(false),
                    ..*mods
                },
                ctx,
            )?
        };
        Ok(Pred::Child(kind, Box::new(inner)))
    };
    Ok(match name {
        "size" => {
            if arg.trim().eq_ignore_ascii_case("unknown") {
                return Ok(Pred::False);
            }
            Pred::Size(range(name, arg, &parse_size)?)
        }
        "dm" => time(TimeField::Modified)?,
        "dc" => time(TimeField::Created)?,
        "da" => time(TimeField::Accessed)?,
        "dch" => time(TimeField::Changed)?,
        "dr" => Pred::DateRun(range(name, arg, &date)?),
        "runcount" => Pred::RunCount(range(name, arg, &parse_number)?),
        "ext" => Pred::And(vec![
            Pred::Kind(KindOnly::Files),
            Pred::Ext {
                exts: ext_set(arg, case),
                case,
            },
        ]),
        "type" => {
            if arg.is_empty() {
                return Err(bad(
                    name,
                    "expects a type (e.g. type:image, type:pdf, type:folder)",
                ));
            }
            Pred::Type(Box::new(TextMatch::literal(
                arg,
                TextMode::Substring,
                false,
                true,
                false,
            )))
        }
        "parent" => {
            let mut path = arg.trim().trim_end_matches('/').as_bytes().to_vec();
            if path.is_empty() && arg.trim().starts_with('/') {
                path = b"/".to_vec();
            }
            let mut norm = Vec::new();
            super::matcher::normalize_into(&path, case, true, false, &mut norm);
            Pred::Parent { path: norm, case }
        }
        "depth" => Pred::Depth(range(name, arg, &parse_number)?),
        "len" => Pred::Len(range(name, arg, &parse_number)?),
        "dupe" => {
            let mut props = Vec::new();
            for p in arg
                .split([';', ','])
                .map(str::trim)
                .filter(|p| !p.is_empty())
            {
                props.push(match p.to_ascii_lowercase().as_str() {
                    "name" => DupeProp::Name,
                    "namepart" | "stem" => DupeProp::NamePart,
                    "ext" | "extension" => DupeProp::Ext,
                    "size" => DupeProp::Size,
                    "dm" | "datemodified" => DupeProp::Modified,
                    "dc" | "datecreated" => DupeProp::Created,
                    other => return Err(bad(name, format!("unknown property {other:?}"))),
                });
            }
            if props.is_empty() {
                props.push(DupeProp::Name);
            }
            Pred::Dupe(props)
        }
        "sizedupe" => no_arg(name, arg, Pred::Dupe(vec![DupeProp::Size]))?,
        "namepartdupe" => no_arg(name, arg, Pred::Dupe(vec![DupeProp::NamePart]))?,
        "dmdupe" => no_arg(name, arg, Pred::Dupe(vec![DupeProp::Modified]))?,
        "dcdupe" => no_arg(name, arg, Pred::Dupe(vec![DupeProp::Created]))?,
        "extdupe" => no_arg(name, arg, Pred::Dupe(vec![DupeProp::Ext]))?,
        "empty" => no_arg(name, arg, Pred::Empty)?,
        "child" => child_pred(ChildKind::Any)?,
        "childfile" => child_pred(ChildKind::Files)?,
        "childfolder" => child_pred(ChildKind::Folders)?,
        "childcount" => Pred::ChildCount(ChildKind::Any, range(name, arg, &parse_number)?),
        "childfilecount" => Pred::ChildCount(ChildKind::Files, range(name, arg, &parse_number)?),
        "childfoldercount" => {
            Pred::ChildCount(ChildKind::Folders, range(name, arg, &parse_number)?)
        }
        "startwith" => Pred::Name(literal(TextMode::Prefix)),
        "endwith" => Pred::Name(literal(TextMode::Suffix)),
        "root" => no_arg(name, arg, Pred::Root)?,
        "content" => {
            if arg.is_empty() {
                return Err(bad(name, "expects text to search for"));
            }
            let m = if mods.regex.unwrap_or(ctx.options.regex) {
                TextMatch::regex(arg, case, true).map_err(QueryError::Regex)?
            } else {
                TextMatch::literal(
                    arg,
                    if mods.whole_word.unwrap_or(false) {
                        TextMode::WholeWord
                    } else {
                        TextMode::Substring
                    },
                    case,
                    true,
                    true,
                )
            };
            Pred::And(vec![
                Pred::Kind(KindOnly::Files),
                Pred::Content(Box::new(m)),
            ])
        }
        "attrib" => Pred::Attrib(parse_attrib(arg).map_err(|m| bad(name, m))?),
        "inode" => Pred::Inode(range(name, arg, &parse_number)?),
        "fsi" => Pred::VolumeIndex(range(name, arg, &parse_number)?),
        "perm" => Pred::Perm(parse_perm(arg).map_err(|m| bad(name, m))?),
        "owner" => Pred::Uid(ids(name, arg, &|n| ctx.users.uid_of(n))?),
        "group" => Pred::Gid(ids(name, arg, &|n| ctx.users.gid_of(n))?),
        "uid" => Pred::Uid(range(name, arg, &parse_number)?),
        "gid" => Pred::Gid(range(name, arg, &parse_number)?),
        "symlink" => no_arg(name, arg, Pred::Symlink)?,
        "hidden" => no_arg(name, arg, Pred::Hidden)?,
        "fstype" => Pred::FsType(
            arg.split(';')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        "volume" => Pred::Volume(Box::new(TextMatch::literal(
            arg,
            TextMode::Substring,
            false,
            true,
            false,
        ))),
        "audio" => macro_pred(AUDIO, arg, mods, ctx, None)?,
        "zip" => macro_pred(ZIP, arg, mods, ctx, None)?,
        "doc" => macro_pred(DOC, arg, mods, ctx, None)?,
        "pic" => macro_pred(PIC, arg, mods, ctx, None)?,
        "video" => macro_pred(VIDEO, arg, mods, ctx, None)?,
        // Linux: executable regular files (any x bit) plus the usual executable extensions.
        "exe" => macro_pred(
            EXE,
            arg,
            mods,
            ctx,
            Some(Pred::And(vec![
                Pred::Attrib(AttribSet {
                    archive: true,
                    ..AttribSet::default()
                }),
                Pred::Perm(PermTest::AnySet(0o111)),
            ])),
        )?,
        "rc" | "tag" | "width" | "height" | "dimensions" | "orientation" | "bitdepth" | "album"
        | "artist" | "genre" | "title" | "track" | "comment" | "year" | "length" | "filelist"
        | "filelistfilename" | "shell" => return Err(QueryError::NotSupported(name.to_owned())),
        user => {
            let Some(expansion) = ctx
                .macros
                .user
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(user))
                .map(|(_, v)| v.clone())
            else {
                return Err(QueryError::NotSupported(user.to_owned()));
            };
            if depth >= MAX_MACRO_DEPTH {
                return Err(QueryError::MacroDepth(user.to_owned()));
            }
            let opts = ParseOptions {
                macros: ctx
                    .macros
                    .user
                    .keys()
                    .map(|k| k.to_ascii_lowercase())
                    .collect(),
            };
            let tree = parse(&expansion, &opts);
            let mut parts = vec![compile(&tree, ctx, depth + 1)?];
            if !arg.is_empty() {
                parts.push(text_pred(
                    arg,
                    &Mods {
                        kind: None,
                        ..*mods
                    },
                    ctx,
                )?);
            }
            Pred::And(parts)
        }
    })
}

/// Flattens and simplifies a predicate tree and orders AND/OR children by cost.
#[must_use]
pub fn simplify(p: Pred) -> Pred {
    match p {
        Pred::And(children) => {
            let mut out = Vec::new();
            for c in children {
                match simplify(c) {
                    Pred::True => {}
                    Pred::False => return Pred::False,
                    Pred::And(inner) => out.extend(inner),
                    other => out.push(other),
                }
            }
            out.sort_by_key(Pred::cost);
            match out.len() {
                0 => Pred::True,
                1 => out.pop().unwrap_or(Pred::True),
                _ => Pred::And(out),
            }
        }
        Pred::Or(children) => {
            let mut out = Vec::new();
            for c in children {
                match simplify(c) {
                    Pred::True => return Pred::True,
                    Pred::False => {}
                    Pred::Or(inner) => out.extend(inner),
                    other => out.push(other),
                }
            }
            out.sort_by_key(Pred::cost);
            match out.len() {
                0 => Pred::False,
                1 => out.pop().unwrap_or(Pred::False),
                _ => Pred::Or(out),
            }
        }
        Pred::Not(c) => match simplify(*c) {
            Pred::True => Pred::False,
            Pred::False => Pred::True,
            Pred::Not(inner) => *inner,
            other => Pred::Not(Box::new(other)),
        },
        Pred::Child(k, c) => Pred::Child(k, Box::new(simplify(*c))),
        Pred::Name(m) if m.matches_all() => Pred::True,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;

    fn ctx() -> CompileContext<'static> {
        static MACROS: std::sync::OnceLock<Macros> = std::sync::OnceLock::new();
        let macros = MACROS.get_or_init(|| {
            let mut m = Macros::default();
            m.user.insert("src".into(), "ext:rs;c;h".into());
            m.user.insert("loop".into(), "loop:".into());
            m
        });
        CompileContext {
            options: SearchOptions::default(),
            clock: Clock::at(1_718_206_200, TimeZone::UTC),
            macros,
            users: &NumericIds,
        }
    }

    fn c(q: &str) -> Result<Pred, QueryError> {
        compile_str(q, &ctx()).map(|c| c.pred)
    }

    #[test]
    fn compiles_common_queries() {
        assert!(matches!(c("").unwrap(), Pred::True));
        assert!(matches!(c("foo").unwrap(), Pred::Name(_)));
        assert!(matches!(c("/usr/lib").unwrap(), Pred::Path(_)));
        assert!(matches!(c("path:foo").unwrap(), Pred::Path(_)));
        assert!(
            matches!(c("*.txt").unwrap(), Pred::Name(m) if matches!(m.mode, TextMode::Glob(_)))
        );
        assert!(matches!(c("*.tx?").unwrap(), Pred::Name(m) if m.is_regex()));
        assert!(matches!(c("size:>1gb").unwrap(), Pred::Size(_)));
        assert!(matches!(
            c("dm:today").unwrap(),
            Pred::Time(TimeField::Modified, _)
        ));
        assert!(matches!(
            c("dc:unknown").unwrap(),
            Pred::TimeUnknown(TimeField::Created)
        ));
        assert!(matches!(c("audio:abba").unwrap(), Pred::And(_)));
        assert!(matches!(c("src:main").unwrap(), Pred::And(_)));
        assert!(matches!(
            c("perm:755").unwrap(),
            Pred::Perm(PermTest::Exact(0o755))
        ));
        assert!(matches!(
            c("perm:u+x").unwrap(),
            Pred::Perm(PermTest::AllSet(0o100))
        ));
        assert!(matches!(
            c("perm:+x").unwrap(),
            Pred::Perm(PermTest::AnySet(0o111))
        ));
        assert!(matches!(
            c("perm:o-w").unwrap(),
            Pred::Perm(PermTest::NoneSet(0o002))
        ));
        assert!(matches!(
            c("perm:rwxr-x---").unwrap(),
            Pred::Perm(PermTest::Exact(0o750))
        ));
        assert!(matches!(c("owner:1000").unwrap(), Pred::Uid(_)));
        assert!(matches!(c("!foo").unwrap(), Pred::Not(_)));
        assert!(matches!(c("file:").unwrap(), Pred::Kind(KindOnly::Files)));
    }

    #[test]
    fn errors_are_reported() {
        assert_eq!(
            c("width:100").unwrap_err(),
            QueryError::NotSupported("width".into())
        );
        assert_eq!(
            c("tag:work").unwrap_err(),
            QueryError::NotSupported("tag".into())
        );
        assert!(matches!(c("size:abc"), Err(QueryError::BadArgument { .. })));
        assert!(matches!(
            c("dm:someday"),
            Err(QueryError::BadArgument { .. })
        ));
        assert!(matches!(c("regex:("), Err(QueryError::Regex(_))));
        assert!(matches!(
            c("owner:nosuchuser"),
            Err(QueryError::UnknownId(_))
        ));
        assert!(matches!(c("empty:x"), Err(QueryError::BadArgument { .. })));
        assert!(matches!(c("loop:"), Err(QueryError::MacroDepth(_))));
    }

    #[test]
    fn simplification() {
        assert!(matches!(c("foo|<>").unwrap(), Pred::True));
        assert!(matches!(c("!<>").unwrap(), Pred::False));
        // Cheap predicates come first in an AND.
        let Pred::And(parts) = c("foo size:>1mb").unwrap() else {
            panic!()
        };
        assert!(matches!(parts[0], Pred::Size(_)));
    }
}
