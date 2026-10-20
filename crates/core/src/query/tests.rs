use super::*;

fn p(s: &str) -> Node {
    parse(s, &ParseOptions::default())
}

fn text(t: &str) -> Node {
    Node::Text {
        text: t.into(),
        mods: Mods::default(),
    }
}

fn func(n: &str, a: &str) -> Node {
    Node::Func {
        name: n.into(),
        arg: a.into(),
        mods: Mods::default(),
    }
}

#[test]
fn operators_and_precedence() {
    assert_eq!(p(""), Node::all());
    assert_eq!(p("foo"), text("foo"));
    assert_eq!(p("foo bar"), Node::And(vec![text("foo"), text("bar")]));
    // OR binds tighter than AND.
    assert_eq!(
        p("a b|c"),
        Node::And(vec![text("a"), Node::Or(vec![text("b"), text("c")])])
    );
    assert_eq!(p("a | b"), Node::Or(vec![text("a"), text("b")]));
    assert_eq!(p("!foo"), Node::Not(Box::new(text("foo"))));
    assert_eq!(
        p("<a b>|c"),
        Node::Or(vec![Node::And(vec![text("a"), text("b")]), text("c")])
    );
    assert_eq!(
        p("!<a|b> c"),
        Node::And(vec![
            Node::Not(Box::new(Node::Or(vec![text("a"), text("b")]))),
            text("c")
        ])
    );
    // Unbalanced brackets are forgiven.
    assert_eq!(p("<a b"), Node::And(vec![text("a"), text("b")]));
    assert_eq!(p("a> b"), Node::And(vec![text("a"), text("b")]));
}

#[test]
fn quotes_and_phrases() {
    assert_eq!(p("\"foo bar\""), text("foo bar"));
    assert_eq!(p("foo\" \"bar"), text("foo bar"));
    assert_eq!(p("\"a|b<c>\""), text("a|b<c>"));
    // Quoting prevents function parsing.
    assert_eq!(p("\"size:1mb\""), text("size:1mb"));
    assert_eq!(p("10:30"), text("10:30"));
}

#[test]
fn modifiers_and_functions() {
    assert_eq!(
        p("case:Foo"),
        Node::Text {
            text: "Foo".into(),
            mods: Mods {
                case: Some(true),
                ..Mods::default()
            }
        }
    );
    assert_eq!(
        p("CASE:regex:^a.*b$"),
        Node::Text {
            text: "^a.*b$".into(),
            mods: Mods {
                case: Some(true),
                regex: Some(true),
                ..Mods::default()
            }
        }
    );
    assert_eq!(p("size:>1mb"), func("size", ">1mb"));
    assert_eq!(p("Size:>=1mb"), func("size", ">=1mb"));
    assert_eq!(p("dm:<2024"), func("dm", "<2024"));
    assert_eq!(p("datemodified:today"), func("dm", "today"));
    assert_eq!(p("size:10mb..100mb"), func("size", "10mb..100mb"));
    assert_eq!(p("ext:jpg;png"), func("ext", "jpg;png"));
    assert_eq!(p("parent:\"/home/a b\""), func("parent", "/home/a b"));
    assert_eq!(p("empty:"), func("empty", ""));
    // Comparison after function colon inside a group.
    assert_eq!(
        p("<size:>1mb ext:iso>"),
        Node::And(vec![func("size", ">1mb"), func("ext", "iso")])
    );
    // file: alone is a filter; file:foo restricts foo.
    let files = Mods {
        kind: Some(KindOnly::Files),
        ..Mods::default()
    };
    assert_eq!(
        p("file:"),
        Node::Text {
            text: String::new(),
            mods: files
        }
    );
    assert_eq!(
        p("file:foo"),
        Node::Text {
            text: "foo".into(),
            mods: files
        }
    );
    // Modifiers apply to an adjacent group.
    let case = Mods {
        case: Some(true),
        ..Mods::default()
    };
    assert_eq!(
        p("case:<A|B>"),
        Node::Or(vec![
            Node::Text {
                text: "A".into(),
                mods: case
            },
            Node::Text {
                text: "B".into(),
                mods: case
            },
        ])
    );
    // Modifier on a function.
    assert_eq!(
        p("case:ext:TXT"),
        Node::Func {
            name: "ext".into(),
            arg: "TXT".into(),
            mods: case
        }
    );
    // Unknown prefixes are text.
    assert_eq!(p("foo:bar"), text("foo:bar"));
    // Quoted part after a modifier is literal even if it looks like a function.
    assert_eq!(
        p("case:\"size:5\""),
        Node::Text {
            text: "size:5".into(),
            mods: case
        }
    );
}

#[test]
fn user_macros_are_recognized() {
    let mut opts = ParseOptions::default();
    opts.macros.insert("src".into());
    assert_eq!(parse("src:main", &opts), func("src", "main"));
}

#[test]
fn printing_round_trips() {
    for q in [
        "foo bar",
        "a b|c",
        "<a b>|c",
        "!<a|b> c",
        "case:Foo nocase:bar",
        "size:>1mb ext:jpg;png",
        "parent:\"/home/a b\" \"x y\"",
        "file: folder:foo",
        "<<a|b> c>|d",
        "!!x",
        "\"size:1mb\"",
        "dm:2024-01-01..2024-02-01",
        "regex:^a|b$",
        "<>",
        "\"\"",
    ] {
        let ast = p(q);
        let printed = ast.to_string();
        assert_eq!(p(&printed), ast, "query {q:?} printed as {printed:?}");
    }
}

mod props {
    use super::*;
    use proptest::prelude::*;

    fn leaf() -> impl Strategy<Value = Node> {
        let word = "[a-zA-Z0-9._:/ |<>!-]{0,8}";
        let mods = (
            proptest::option::of(any::<bool>()),
            proptest::option::of(any::<bool>()),
            proptest::option::of(any::<bool>()),
            proptest::option::of(prop_oneof![
                Just(KindOnly::Files),
                Just(KindOnly::Folders),
                Just(KindOnly::Any)
            ]),
        )
            .prop_map(|(case, regex, path, kind)| Mods {
                case,
                regex,
                path,
                kind,
                ..Mods::default()
            });
        prop_oneof![
            (word, mods.clone()).prop_map(|(t, mods)| Node::Text {
                text: t.replace('"', ""),
                mods
            }),
            (
                prop_oneof![
                    Just("size"),
                    Just("ext"),
                    Just("dm"),
                    Just("parent"),
                    Just("dupe")
                ],
                word,
                mods
            )
                .prop_map(|(n, a, mods)| Node::Func {
                    name: n.into(),
                    arg: a.replace('"', ""),
                    mods
                }),
        ]
    }

    fn tree() -> impl Strategy<Value = Node> {
        leaf().prop_recursive(4, 32, 4, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..4).prop_map(Node::And),
                proptest::collection::vec(inner.clone(), 2..4).prop_map(Node::Or),
                inner.prop_map(|n| Node::Not(Box::new(n))),
            ]
        })
    }

    proptest! {
        #[test]
        fn parse_print_parse(ast in tree()) {
            let ast = normalize(ast);
            let printed = ast.to_string();
            let reparsed = parse(&printed, &ParseOptions::default());
            prop_assert_eq!(&reparsed, &ast, "printed: {}", printed);
        }

        #[test]
        fn parser_never_panics(s in "\\PC{0,64}") {
            let ast = parse(&s, &ParseOptions::default());
            let _ = ast.to_string();
        }
    }
}
