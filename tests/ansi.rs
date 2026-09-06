//! Integration tests for `ansi`.
//!
//! Anchors: SGR goldens asserted byte-exact both ways (build → parse →
//! re-apply must reproduce the style); tokenizer fixtures from xterm/ECMA-48
//! sequences, each also replayed byte-at-a-time to prove chunk-split
//! invariance; and the screen renderer verified end-to-end by applying its
//! own diff output through the parser to an interpreted screen: the diff is
//! correct iff the interpreted result equals the target frame.

use ansi::{Cell, Color, Parser, Screen, Style, Token};

fn style(f: impl Fn(&mut Style)) -> Style {
    let mut s = Style::default();
    f(&mut s);
    s
}

// --- SGR building and application -----------------------------------------

#[test]
fn sgr_goldens() {
    assert_eq!(Style::default().sgr(), "\x1b[0m");
    assert_eq!(style(|s| s.bold = true).sgr(), "\x1b[0;1m");
    assert_eq!(
        style(|s| {
            s.bold = true;
            s.fg = Color::Indexed(1);
        })
        .sgr(),
        "\x1b[0;1;31m"
    );
    assert_eq!(style(|s| s.fg = Color::Indexed(9)).sgr(), "\x1b[0;91m");
    assert_eq!(style(|s| s.bg = Color::Indexed(4)).sgr(), "\x1b[0;44m");
    assert_eq!(style(|s| s.bg = Color::Indexed(12)).sgr(), "\x1b[0;104m");
    assert_eq!(
        style(|s| s.fg = Color::Indexed(200)).sgr(),
        "\x1b[0;38;5;200m"
    );
    assert_eq!(
        style(|s| s.bg = Color::Rgb(1, 2, 3)).sgr(),
        "\x1b[0;48;2;1;2;3m"
    );
    assert_eq!(
        style(|s| {
            s.italic = true;
            s.underline = true;
            s.strike = true;
        })
        .sgr(),
        "\x1b[0;3;4;9m"
    );
}

#[test]
fn transition_is_empty_for_equal_styles() {
    let s = style(|s| s.bold = true);
    assert_eq!(s.transition_to(&s), "");
}

#[test]
fn transition_additive_when_only_adding() {
    let from = style(|s| s.bold = true);
    let to = style(|s| {
        s.bold = true;
        s.fg = Color::Indexed(2);
    });
    assert_eq!(from.transition_to(&to), "\x1b[32m");
    let to2 = style(|s| {
        s.bold = true;
        s.underline = true;
    });
    assert_eq!(from.transition_to(&to2), "\x1b[4m");
}

#[test]
fn transition_resets_when_removing() {
    let from = style(|s| {
        s.bold = true;
        s.fg = Color::Indexed(2);
    });
    assert_eq!(from.transition_to(&Style::default()), "\x1b[0m");
    let to = style(|s| s.fg = Color::Indexed(2));
    assert_eq!(from.transition_to(&to), "\x1b[0;32m");
}

/// Build → parse → re-apply reproduces the style, for a spread of styles and
/// transitions between them.
#[test]
fn sgr_round_trips_through_parser() {
    let styles = [
        Style::default(),
        style(|s| s.bold = true),
        style(|s| {
            s.dim = true;
            s.reverse = true;
        }),
        style(|s| s.fg = Color::Indexed(3)),
        style(|s| {
            s.fg = Color::Indexed(15);
            s.bg = Color::Indexed(236);
        }),
        style(|s| {
            s.fg = Color::Rgb(10, 20, 30);
            s.underline = true;
        }),
        style(|s| {
            s.bg = Color::Rgb(255, 0, 128);
            s.italic = true;
            s.strike = true;
        }),
    ];
    for a in &styles {
        // absolute form
        let mut p = Parser::new();
        let toks = p.feed(a.sgr().as_bytes());
        let mut applied = style(|s| s.bold = true); // arbitrary prior state
        apply_sgr_tokens(&toks, &mut applied);
        assert_eq!(&applied, a, "absolute sgr for {a:?}");
        // transition form, from every other style
        for b in &styles {
            let mut p = Parser::new();
            let toks = p.feed(a.transition_to(b).as_bytes());
            let mut applied = *a;
            apply_sgr_tokens(&toks, &mut applied);
            assert_eq!(&applied, b, "transition {a:?} -> {b:?}");
        }
    }
}

fn apply_sgr_tokens(tokens: &[Token], style: &mut Style) {
    for t in tokens {
        if let Token::Csi {
            final_byte: 'm',
            params,
            private: None,
            ..
        } = t
        {
            style.apply_sgr(params);
        }
    }
}

// --- tokenizer -------------------------------------------------------------

fn csi(params: &[u16], final_byte: char) -> Token {
    Token::Csi {
        private: None,
        params: params.to_vec(),
        intermediates: Vec::new(),
        final_byte,
    }
}

/// (input bytes, expected tokens): the fixture table.
fn fixtures() -> Vec<(&'static [u8], Vec<Token>)> {
    vec![
        (b"hello", vec![Token::Text("hello".into())]),
        (
            b"a\nb",
            vec![
                Token::Text("a".into()),
                Token::Control(b'\n'),
                Token::Text("b".into()),
            ],
        ),
        (
            b"\r\n\t\x07",
            vec![
                Token::Control(b'\r'),
                Token::Control(b'\n'),
                Token::Control(b'\t'),
                Token::Control(0x07),
            ],
        ),
        (b"\x1b[m", vec![csi(&[], 'm')]),
        (b"\x1b[0m", vec![csi(&[0], 'm')]),
        (
            b"\x1b[1;31mred",
            vec![csi(&[1, 31], 'm'), Token::Text("red".into())],
        ),
        (b"\x1b[38;5;200m", vec![csi(&[38, 5, 200], 'm')]),
        (b"\x1b[2;5H", vec![csi(&[2, 5], 'H')]),
        (b"\x1b[H", vec![csi(&[], 'H')]),
        (b"\x1b[2J\x1b[K", vec![csi(&[2], 'J'), csi(&[], 'K')]),
        // missing params parse as 0
        (b"\x1b[;5H", vec![csi(&[0, 5], 'H')]),
        // colon sub-parameters are truncated at the colon
        (b"\x1b[4:3m", vec![csi(&[4], 'm')]),
        // private markers
        (
            b"\x1b[?25l",
            vec![Token::Csi {
                private: Some('?'),
                params: vec![25],
                intermediates: Vec::new(),
                final_byte: 'l',
            }],
        ),
        (
            b"\x1b[>0c",
            vec![Token::Csi {
                private: Some('>'),
                params: vec![0],
                intermediates: Vec::new(),
                final_byte: 'c',
            }],
        ),
        // CSI with intermediate byte
        (
            b"\x1b[!p",
            vec![Token::Csi {
                private: None,
                params: vec![],
                intermediates: vec![b'!'],
                final_byte: 'p',
            }],
        ),
        // plain escapes
        (
            b"\x1b7\x1b8",
            vec![
                Token::Esc {
                    intermediates: vec![],
                    final_byte: b'7',
                },
                Token::Esc {
                    intermediates: vec![],
                    final_byte: b'8',
                },
            ],
        ),
        (
            b"\x1b(B",
            vec![Token::Esc {
                intermediates: vec![b'('],
                final_byte: b'B',
            }],
        ),
        // OSC, both terminators
        (b"\x1b]0;title\x07", vec![Token::Osc("0;title".into())]),
        (b"\x1b]0;title\x1b\\", vec![Token::Osc("0;title".into())]),
        // DCS swallowed raw
        (
            b"\x1bPdata\x1b\\",
            vec![Token::Other {
                intro: b'P',
                data: b"data".to_vec(),
            }],
        ),
        // C0 executed inside a CSI sequence, sequence continues
        (
            b"\x1b[1\n;2H",
            vec![Token::Control(b'\n'), csi(&[1, 2], 'H')],
        ),
        // CAN aborts a sequence; following text is plain
        (b"\x1b[1;2\x18ok", vec![Token::Text("ok".into())]),
        // stray ESC restarts a sequence
        (b"\x1b[1\x1b[2m", vec![csi(&[2], 'm')]),
        // UTF-8 text with escapes around it
        (
            "héllo — 🎉".as_bytes(),
            vec![Token::Text("héllo — 🎉".into())],
        ),
        (
            b"\x1b[31m\xc3\xa9\x1b[0m",
            vec![csi(&[31], 'm'), Token::Text("é".into()), csi(&[0], 'm')],
        ),
        // invalid UTF-8 becomes U+FFFD, never breaks the stream
        (b"a\xffb", vec![Token::Text("a\u{FFFD}b".into())]),
        (b"a\xc3b", vec![Token::Text("a\u{FFFD}b".into())]),
    ]
}

/// Coalesce adjacent Text tokens so chunked and one-shot feeds compare equal.
fn normalize(tokens: Vec<Token>) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    for t in tokens {
        match (out.last_mut(), t) {
            (Some(Token::Text(a)), Token::Text(b)) => a.push_str(&b),
            (_, t) => out.push(t),
        }
    }
    out
}

#[test]
fn tokenizer_fixtures_one_shot() {
    for (input, expected) in fixtures() {
        let mut p = Parser::new();
        let got = normalize(p.feed(input));
        assert_eq!(
            &got,
            &expected,
            "input: {:?}",
            String::from_utf8_lossy(input)
        );
    }
}

/// Feeding byte-at-a-time must produce the same tokens as one shot; chunk
/// boundaries can split escapes and multibyte characters anywhere.
#[test]
fn tokenizer_fixtures_split_at_every_byte() {
    for (input, expected) in fixtures() {
        let mut p = Parser::new();
        let mut got = Vec::new();
        for &b in input {
            got.extend(p.feed(&[b]));
        }
        let got = normalize(got);
        assert_eq!(
            &got,
            &expected,
            "bytewise: {:?}",
            String::from_utf8_lossy(input)
        );
    }
}

#[test]
fn text_flushes_at_end_of_each_feed() {
    let mut p = Parser::new();
    assert_eq!(p.feed(b"ab"), vec![Token::Text("ab".into())]);
    assert_eq!(p.feed(b"cd"), vec![Token::Text("cd".into())]);
}

#[test]
fn partial_utf8_is_held_across_feeds() {
    let mut p = Parser::new();
    let bytes = "é".as_bytes();
    assert_eq!(p.feed(&bytes[..1]), vec![]);
    assert_eq!(p.feed(&bytes[1..]), vec![Token::Text("é".into())]);
}

// --- screen diff, verified through the parser ------------------------------

/// Apply rendered bytes to a screen the way a terminal would (CUP, SGR,
/// erase-display, printable text). This is the verification oracle: a diff
/// is correct iff applying it reproduces the target frame.
fn apply(screen: &mut Screen, bytes: &[u8]) {
    let mut p = Parser::new();
    let mut style = Style::default();
    let (mut row, mut col) = (0usize, 0usize);
    for t in p.feed(bytes) {
        match t {
            Token::Text(s) => {
                for ch in s.chars() {
                    screen.set(row, col, Cell { ch, style });
                    col += 1;
                    if col >= screen.cols() {
                        col = screen.cols() - 1; // park at edge; diff always CUPs after edge writes
                    }
                }
            }
            Token::Csi {
                final_byte: 'm',
                params,
                private: None,
                ..
            } => style.apply_sgr(&params),
            Token::Csi {
                final_byte: 'H',
                params,
                private: None,
                ..
            } => {
                row = (*params.first().unwrap_or(&1)).max(1) as usize - 1;
                col = (*params.get(1).unwrap_or(&1)).max(1) as usize - 1;
            }
            Token::Csi {
                final_byte: 'J',
                params,
                private: None,
                ..
            } => {
                if params.first() == Some(&2) {
                    let (r, c) = (screen.rows(), screen.cols());
                    for rr in 0..r {
                        for cc in 0..c {
                            screen.set(rr, cc, Cell::default());
                        }
                    }
                }
            }
            _ => panic!("renderer emitted unexpected token: {t:?}"),
        }
    }
    screen.cursor = (row, col);
}

fn check_diff(old: &Screen, new: &Screen) {
    let bytes = old.diff(new);
    let mut replay = old.clone();
    apply(&mut replay, &bytes);
    assert_eq!(
        &replay,
        new,
        "diff bytes: {:?}",
        String::from_utf8_lossy(&bytes)
    );
}

#[test]
fn diff_of_equal_screens_is_empty() {
    let mut s = Screen::new(3, 10);
    s.write_str(1, 2, "hi", Style::default());
    assert_eq!(s.diff(&s.clone()), b"");
}

#[test]
fn diff_reproduces_target_frames() {
    let blank = Screen::new(4, 12);

    let mut a = blank.clone();
    a.write_str(0, 0, "hello", Style::default());
    check_diff(&blank, &a);

    // single-cell change
    let mut b = a.clone();
    b.set(
        0,
        1,
        Cell {
            ch: 'a',
            style: Style::default(),
        },
    );
    check_diff(&a, &b);

    // styled run + second row
    let mut c = b.clone();
    let red = style(|s| {
        s.fg = Color::Indexed(1);
        s.bold = true;
    });
    c.write_str(2, 3, "warn!", red);
    c.cursor = (3, 0);
    check_diff(&b, &c);

    // style-only change on existing text
    let mut d = c.clone();
    d.write_str(2, 3, "warn!", style(|s| s.fg = Color::Indexed(2)));
    check_diff(&c, &d);

    // write through the last column
    let mut e = d.clone();
    e.write_str(1, 7, "edge!", Style::default()); // cols 7..12 == right edge
    check_diff(&d, &e);

    // clearing back to blank (removals)
    check_diff(&e, &blank);
}

#[test]
fn render_full_reproduces_frame_from_any_prior_state() {
    let mut garbage = Screen::new(3, 8);
    garbage.write_str(0, 0, "XXXXXXXX", style(|s| s.reverse = true));
    garbage.write_str(2, 0, "YYYYYYYY", Style::default());

    let mut target = Screen::new(3, 8);
    target.write_str(1, 1, "ok", style(|s| s.underline = true));
    target.cursor = (1, 3);

    let mut replay = garbage.clone();
    apply(&mut replay, &target.render_full());
    assert_eq!(replay, target);
}

#[test]
fn dimension_mismatch_falls_back_to_full_repaint() {
    let old = Screen::new(2, 4);
    let mut new = Screen::new(3, 6);
    new.write_str(0, 0, "resize", Style::default());
    let bytes = old.diff(&new);
    let mut replay = Screen::new(3, 6);
    replay.write_str(1, 1, "junk", Style::default()); // 2J must wipe this
    apply(&mut replay, &bytes);
    assert_eq!(replay, new);
}

#[test]
fn diff_golden_minimal_bytes() {
    let mut old = Screen::new(2, 10);
    old.write_str(0, 0, "hello", Style::default());
    let mut new = old.clone();
    new.set(
        0,
        1,
        Cell {
            ch: 'a',
            style: Style::default(),
        },
    );
    // one CUP, one char, no SGR needed, park cursor at (1,1)
    assert_eq!(old.diff(&new), b"\x1b[1;2Ha\x1b[1;1H");
}
