//! Incremental VT/ANSI tokenizer.
//!
//! Two APIs over one state machine, so they can never disagree:
//!
//! * [`Parser::feed_with`] is the engine. It calls a closure with an
//!   [`Event`] that **borrows** — text runs point into the caller's buffer,
//!   sequence payloads into the parser's own reused scratch — so tokenizing a
//!   stream allocates nothing after the first few sequences.
//! * [`Parser::feed`] is the owned convenience API: the same events collected
//!   into a `Vec<Token>`, with text decoded as UTF-8 (invalid bytes become
//!   U+FFFD). It costs a `String`/`Vec` per token, which is why the engine
//!   exists.
//!
//! Both accept a byte stream in chunks split at *any* boundary (mid-escape
//! sequence, mid-UTF-8 character). The state machine follows the classic
//! VT500-series design: C0 controls are *executed* (emitted as
//! [`Event::Control`]) even when they arrive inside an escape sequence,
//! `CAN`/`SUB` abort a sequence in progress, and a stray `ESC` restarts one.
//! Malformed sequences are dropped, never emitted as text.
//!
//! # Choosing between them
//!
//! ```
//! # use ansi::{Event, Parser};
//! let mut p = Parser::new();
//! let mut printable = 0usize;
//! p.feed_with(b"hi\x1b[1;31mred", |event| {
//!     if let Event::Text(bytes) = event {
//!         printable += bytes.len();
//!     }
//! });
//! assert_eq!(printable, 5); // "hi" + "red", no allocation
//! ```

/// One parsed unit of terminal output, owned.
///
/// [`Event`] is the same information borrowed; this is what [`Parser::feed`]
/// builds from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// A run of printable text (valid UTF-8; invalid input bytes become
    /// U+FFFD). Runs are flushed at each control byte and at the end of each
    /// `feed` call, so adjacent `Text` tokens are equivalent to one.
    Text(String),
    /// A C0 control byte executed on its own (BEL, BS, TAB, LF, CR, ...) or
    /// DEL. `ESC` never appears here; it introduces a sequence.
    Control(u8),
    /// `ESC [ ...`: a CSI sequence with an optional private marker (`?`, `>`, `<`,
    /// `=`), numeric parameters, intermediate bytes, and the final byte.
    /// Missing parameters parse as 0; colon sub-parameters are truncated.
    Csi {
        private: Option<char>,
        params: Vec<u16>,
        intermediates: Vec<u8>,
        final_byte: char,
    },
    /// A non-CSI escape: `ESC` + optional intermediates + final byte
    /// (e.g. `ESC 7`, `ESC ( B`).
    Esc {
        intermediates: Vec<u8>,
        final_byte: u8,
    },
    /// `ESC ] ...`: an OSC string, terminated by BEL or ST (`ESC \`).
    /// Payload bytes are decoded as UTF-8 (lossily).
    Osc(String),
    /// A DCS/SOS/PM/APC string (`intro` is `P`, `X`, `^`, or `_`), swallowed
    /// whole and surfaced raw rather than interpreted.
    Other { intro: u8, data: Vec<u8> },
}

/// One parsed unit, borrowed: nothing here is owned or copied.
///
/// The lifetime is per call, not per `feed_with`: [`Event::Text`] borrows the
/// bytes passed in, while sequence payloads borrow the parser's scratch
/// buffers, which the next sequence reuses. Keep what you need before
/// returning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    /// A run of printable bytes, exactly as they arrived: **not** validated as
    /// UTF-8, and a multi-byte character split across two `feed_with` calls
    /// arrives as the tail of one run and the head of the next. Callers that
    /// need `str` decode it themselves; [`Parser::feed`] does that and
    /// replaces invalid bytes with U+FFFD.
    Text(&'a [u8]),
    /// A C0 control byte executed on its own, or DEL.
    Control(u8),
    /// A CSI sequence. `params` and `intermediates` borrow parser scratch.
    Csi {
        private: Option<u8>,
        params: &'a [u16],
        intermediates: &'a [u8],
        final_byte: u8,
    },
    /// A non-CSI escape sequence.
    Esc {
        intermediates: &'a [u8],
        final_byte: u8,
    },
    /// An OSC string's payload, without the introducer or terminator.
    Osc(&'a [u8]),
    /// A DCS/SOS/PM/APC string's payload.
    Other { intro: u8, data: &'a [u8] },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    Str, // DCS / SOS / PM / APC
}

/// Cap accumulated sequence payloads so a hostile stream cannot grow memory
/// without bound; sequences exceeding this are dropped.
const MAX_SEQ: usize = 4096;
/// Parameters kept per CSI sequence; later ones are ignored, as terminals do.
const MAX_PARAMS: usize = 32;
/// Intermediate bytes kept per sequence.
const MAX_INTERMEDIATES: usize = 8;

/// The incremental parser. Create one per stream and feed it each chunk as it
/// arrives, with [`feed_with`](Parser::feed_with) or [`feed`](Parser::feed).
#[derive(Debug, Default)]
pub struct Parser {
    state: State,
    // CSI scratch, all reused between sequences.
    private: Option<u8>,
    params: [u16; MAX_PARAMS],
    params_len: usize,
    param_value: u32,
    param_open: bool,
    param_sub: bool,
    param_bytes: usize,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediates_len: usize,
    // String (OSC/DCS/SOS/PM/APC) scratch.
    str_intro: u8,
    str_data: Vec<u8>,
    str_esc: bool,
    // Used only by `feed`: a UTF-8 character split across calls.
    pending_utf8: Vec<u8>,
}

impl Parser {
    /// A parser in the ground state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Tokenize `input`, calling `f` once per [`Event`], allocating nothing.
    ///
    /// Partial sequences are held for the next call. A text run in progress is
    /// always delivered before returning, so no input byte is buffered except
    /// inside an unfinished escape sequence.
    pub fn feed_with<F: FnMut(Event<'_>)>(&mut self, input: &[u8], mut f: F) {
        let mut run: Option<usize> = None;
        let mut i = 0;
        while i < input.len() {
            let b = input[i];
            match self.state {
                State::Ground => match b {
                    0x1B => {
                        flush_run(&mut run, input, i, &mut f);
                        self.begin(State::Escape);
                    }
                    0x00..=0x1F | 0x7F => {
                        flush_run(&mut run, input, i, &mut f);
                        f(Event::Control(b));
                    }
                    _ => {
                        if run.is_none() {
                            run = Some(i);
                        }
                    }
                },
                State::Escape => match b {
                    b'[' => self.begin(State::Csi),
                    b']' => self.begin(State::Osc),
                    b'P' | b'X' | b'^' | b'_' => {
                        self.begin(State::Str);
                        self.str_intro = b;
                    }
                    0x20..=0x2F => {
                        self.push_intermediate(b);
                        self.state = State::EscapeIntermediate;
                    }
                    0x30..=0x7E => {
                        f(Event::Esc {
                            intermediates: &[],
                            final_byte: b,
                        });
                        self.state = State::Ground;
                    }
                    0x1B => self.begin(State::Escape),
                    0x18 | 0x1A => self.state = State::Ground, // CAN / SUB abort
                    _ => f(Event::Control(b)),                 // execute C0 within sequence
                },
                State::EscapeIntermediate => match b {
                    0x20..=0x2F => self.push_intermediate(b),
                    0x30..=0x7E => {
                        f(Event::Esc {
                            intermediates: &self.intermediates[..self.intermediates_len],
                            final_byte: b,
                        });
                        self.state = State::Ground;
                    }
                    0x1B => self.begin(State::Escape),
                    0x18 | 0x1A => self.state = State::Ground,
                    _ => f(Event::Control(b)),
                },
                State::Csi => match b {
                    b'0'..=b'9' | b';' | b':' => self.param_byte(b),
                    0x3C..=0x3F => {
                        if self.private.is_none() && self.param_bytes == 0 {
                            self.private = Some(b);
                        } // later private markers are ignored, as most terminals do
                    }
                    0x20..=0x2F => self.push_intermediate(b),
                    0x40..=0x7E => {
                        self.close_param();
                        f(Event::Csi {
                            private: self.private,
                            params: &self.params[..self.params_len],
                            intermediates: &self.intermediates[..self.intermediates_len],
                            final_byte: b,
                        });
                        self.private = None;
                        self.state = State::Ground;
                    }
                    0x1B => self.begin(State::Escape),
                    0x18 | 0x1A => self.state = State::Ground,
                    _ => f(Event::Control(b)),
                },
                State::Osc | State::Str => {
                    let is_osc = self.state == State::Osc;
                    if self.str_esc {
                        self.str_esc = false;
                        if b == b'\\' {
                            self.dispatch_string(is_osc, &mut f);
                            i += 1;
                            continue;
                        }
                        self.push_str_byte(0x1B);
                        // fall through: `b` is ordinary payload
                    }
                    match b {
                        0x07 => self.dispatch_string(is_osc, &mut f), // BEL terminator
                        0x1B => self.str_esc = true,
                        _ => self.push_str_byte(b),
                    }
                }
            }
            i += 1;
        }
        flush_run(&mut run, input, input.len(), &mut f);
    }

    /// Feed a chunk of bytes; returns the tokens completed by this chunk.
    ///
    /// Any accumulated text run is flushed at the end of the call; partial
    /// escape sequences and partial UTF-8 characters are held for the next.
    /// This is [`feed_with`](Parser::feed_with) with the events copied into
    /// owned [`Token`]s; prefer the engine where the allocation matters.
    pub fn feed(&mut self, input: &[u8]) -> Vec<Token> {
        let mut out = Vec::new();
        let mut text = String::new();
        let mut pending = std::mem::take(&mut self.pending_utf8);

        self.feed_with(input, |event| match event {
            Event::Text(bytes) => decode_text(bytes, &mut pending, &mut text),
            other => {
                if !pending.is_empty() {
                    // A truncated UTF-8 sequence: the held bytes were invalid.
                    text.push('\u{FFFD}');
                    pending.clear();
                }
                if !text.is_empty() {
                    out.push(Token::Text(std::mem::take(&mut text)));
                }
                out.push(own(other));
            }
        });

        if !text.is_empty() {
            out.push(Token::Text(text));
        }
        self.pending_utf8 = pending;
        out
    }

    fn begin(&mut self, state: State) {
        self.state = state;
        self.intermediates_len = 0;
        self.params_len = 0;
        self.param_value = 0;
        self.param_open = false;
        self.param_sub = false;
        self.param_bytes = 0;
        self.private = None;
        self.str_data.clear();
        self.str_esc = false;
    }

    fn push_intermediate(&mut self, b: u8) {
        if self.intermediates_len < MAX_INTERMEDIATES {
            self.intermediates[self.intermediates_len] = b;
            self.intermediates_len += 1;
        }
    }

    fn push_str_byte(&mut self, b: u8) {
        if self.str_data.len() < MAX_SEQ {
            self.str_data.push(b);
        }
    }

    /// One byte of a CSI parameter list: a digit, `;` or `:`.
    fn param_byte(&mut self, b: u8) {
        if self.param_bytes >= MAX_SEQ {
            self.state = State::Ground; // oversized: drop the sequence
            return;
        }
        self.param_bytes += 1;
        self.param_open = true;
        match b {
            b';' => {
                self.store_param();
                self.param_value = 0;
                self.param_sub = false;
            }
            b':' => self.param_sub = true, // sub-parameters are truncated
            _ => {
                if !self.param_sub {
                    let digit = u32::from(b - b'0');
                    self.param_value = (self.param_value * 10 + digit).min(u16::MAX as u32);
                }
            }
        }
    }

    fn store_param(&mut self) {
        if self.params_len < MAX_PARAMS {
            self.params[self.params_len] = self.param_value as u16;
            self.params_len += 1;
        }
    }

    /// Close the parameter list at the final byte.
    fn close_param(&mut self) {
        if self.param_open {
            self.store_param();
            self.param_open = false;
        }
    }

    fn dispatch_string<F: FnMut(Event<'_>)>(&mut self, is_osc: bool, f: &mut F) {
        if is_osc {
            f(Event::Osc(&self.str_data));
        } else {
            f(Event::Other {
                intro: self.str_intro,
                data: &self.str_data,
            });
        }
        self.str_data.clear();
        self.state = State::Ground;
    }
}

/// Deliver the text run that ends at `end`, if any.
fn flush_run<F: FnMut(Event<'_>)>(run: &mut Option<usize>, input: &[u8], end: usize, f: &mut F) {
    if let Some(start) = run.take() {
        if start < end {
            f(Event::Text(&input[start..end]));
        }
    }
}

/// Copy a borrowed event into an owned token. Text is handled by the caller,
/// which decodes UTF-8 across chunk boundaries.
fn own(event: Event<'_>) -> Token {
    match event {
        Event::Text(bytes) => Token::Text(String::from_utf8_lossy(bytes).into_owned()),
        Event::Control(b) => Token::Control(b),
        Event::Csi {
            private,
            params,
            intermediates,
            final_byte,
        } => Token::Csi {
            private: private.map(char::from),
            params: params.to_vec(),
            intermediates: intermediates.to_vec(),
            final_byte: char::from(final_byte),
        },
        Event::Esc {
            intermediates,
            final_byte,
        } => Token::Esc {
            intermediates: intermediates.to_vec(),
            final_byte,
        },
        Event::Osc(data) => Token::Osc(String::from_utf8_lossy(data).into_owned()),
        Event::Other { intro, data } => Token::Other {
            intro,
            data: data.to_vec(),
        },
    }
}

/// Append raw text bytes to `text` as UTF-8, holding a character split across
/// calls in `pending` and replacing anything invalid with U+FFFD.
fn decode_text(bytes: &[u8], pending: &mut Vec<u8>, text: &mut String) {
    for &b in bytes {
        if !pending.is_empty() {
            if (0x80..0xC0).contains(&b) {
                pending.push(b);
                if pending.len() == utf8_len(pending[0]) {
                    match std::str::from_utf8(pending) {
                        Ok(s) => text.push_str(s),
                        Err(_) => text.push('\u{FFFD}'),
                    }
                    pending.clear();
                }
                continue;
            }
            // Truncated sequence: the pending bytes were invalid.
            text.push('\u{FFFD}');
            pending.clear();
            // fall through to process `b` normally
        }
        match b {
            0x20..=0x7E => text.push(b as char),
            0x80..=0xBF => text.push('\u{FFFD}'), // stray continuation
            _ => {
                if utf8_len(b) == 1 {
                    text.push('\u{FFFD}'); // invalid lead (0xF8+)
                } else {
                    pending.push(b);
                }
            }
        }
    }
}

/// Expected total length of a UTF-8 sequence from its lead byte (1 for
/// invalid leads, so they consume exactly themselves).
fn utf8_len(lead: u8) -> usize {
    match lead {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(input: &[u8]) -> Vec<Token> {
        let mut p = Parser::new();
        let mut out = Vec::new();
        p.feed_with(input, |e| out.push(own(e)));
        out
    }

    #[test]
    fn engine_and_feed_agree_on_every_chunk_split() {
        let corpus: &[&[u8]] = &[
            b"hello world\r\n",
            b"\x1b[1;31mred\x1b[0m",
            b"\x1b[?25l\x1b[>4;2m\x1b[38;2;10;20;30mx",
            b"\x1b]0;title\x07after\x1b]133;A\x1b\\done",
            b"\x1bPq#0;2\x1b\\tail",
            b"\x1b7\x1b(B\x1b[12\x18aborted",
            "caf\u{e9} \u{1F600} \u{4e2d}\u{6587}".as_bytes(),
            &[0xE2, 0x82, b'x', 0xFF, 0x80, b'y'],
        ];
        for input in corpus {
            let whole = {
                let mut p = Parser::new();
                p.feed(input)
            };
            for split in 1..input.len() {
                let mut p = Parser::new();
                let mut got = p.feed(&input[..split]);
                got.extend(p.feed(&input[split..]));
                assert_eq!(merge_text(&got), merge_text(&whole), "split at {split}");
            }
        }
    }

    /// Adjacent text tokens are equivalent to one, so compare merged runs.
    fn merge_text(tokens: &[Token]) -> Vec<Token> {
        let mut out: Vec<Token> = Vec::new();
        for t in tokens {
            match (out.last_mut(), t) {
                (Some(Token::Text(prev)), Token::Text(next)) => prev.push_str(next),
                _ => out.push(t.clone()),
            }
        }
        out
    }

    #[test]
    fn text_events_borrow_the_input_verbatim() {
        let mut p = Parser::new();
        let mut runs: Vec<Vec<u8>> = Vec::new();
        p.feed_with(b"ab\x1b[0mcd\xFF", |e| {
            if let Event::Text(b) = e {
                runs.push(b.to_vec());
            }
        });
        // Raw bytes, including the invalid 0xFF, and no U+FFFD substitution.
        assert_eq!(runs, vec![b"ab".to_vec(), b"cd\xFF".to_vec()]);
    }

    #[test]
    fn parameters_match_the_owned_api() {
        assert_eq!(
            events(b"\x1b[m"),
            vec![Token::Csi {
                private: None,
                params: vec![],
                intermediates: vec![],
                final_byte: 'm'
            }]
        );
        assert_eq!(
            events(b"\x1b[;1;;2m"),
            vec![Token::Csi {
                private: None,
                params: vec![0, 1, 0, 2],
                intermediates: vec![],
                final_byte: 'm'
            }]
        );
        // Colon sub-parameters are truncated, not split.
        assert_eq!(
            events(b"\x1b[38:2:10:20:30;1m"),
            vec![Token::Csi {
                private: None,
                params: vec![38, 1],
                intermediates: vec![],
                final_byte: 'm'
            }]
        );
        // Values saturate at u16::MAX.
        assert_eq!(
            events(b"\x1b[999999m"),
            vec![Token::Csi {
                private: None,
                params: vec![u16::MAX],
                intermediates: vec![],
                final_byte: 'm'
            }]
        );
        // Only the first private marker counts, and only before parameters.
        assert_eq!(
            events(b"\x1b[?>25h"),
            vec![Token::Csi {
                private: Some('?'),
                params: vec![25],
                intermediates: vec![],
                final_byte: 'h'
            }]
        );
    }

    #[test]
    fn more_than_max_params_are_ignored_not_dropped() {
        let mut input = b"\x1b[".to_vec();
        for i in 0..40 {
            if i > 0 {
                input.push(b';');
            }
            input.extend_from_slice(b"7");
        }
        input.push(b'm');
        let Token::Csi { params, .. } = &events(&input)[0] else {
            panic!("expected a CSI");
        };
        assert_eq!(params.len(), MAX_PARAMS);
        assert!(params.iter().all(|&p| p == 7));
    }

    #[test]
    fn oversized_parameter_lists_drop_the_sequence() {
        let mut input = b"\x1b[".to_vec();
        input.extend(std::iter::repeat(b'1').take(MAX_SEQ + 8));
        input.push(b'm');
        // Dropped: nothing is emitted as a CSI, and the tail returns to ground.
        let tokens = events(&input);
        assert!(!tokens.iter().any(|t| matches!(t, Token::Csi { .. })));
    }

    #[test]
    fn strings_cap_their_payload() {
        let mut input = b"\x1b]0;".to_vec();
        input.extend(std::iter::repeat(b'x').take(MAX_SEQ + 100));
        input.push(0x07);
        let Token::Osc(payload) = &events(&input)[0] else {
            panic!("expected an OSC");
        };
        assert_eq!(payload.len(), MAX_SEQ);
    }

    #[test]
    fn scratch_is_reused_across_sequences() {
        let mut p = Parser::new();
        let mut seen = Vec::new();
        p.feed_with(b"\x1b[1;2m\x1b[?7h\x1b]0;t\x07", |e| seen.push(own(e)));
        assert_eq!(seen.len(), 3);
        // A second sequence must not inherit the first one's parameters,
        // private marker or payload.
        assert_eq!(
            seen[1],
            Token::Csi {
                private: Some('?'),
                params: vec![7],
                intermediates: vec![],
                final_byte: 'h'
            }
        );
        assert_eq!(seen[2], Token::Osc("0;t".into()));
    }

    #[test]
    fn a_partial_sequence_survives_the_chunk_boundary() {
        let mut p = Parser::new();
        let mut seen = Vec::new();
        for chunk in [&b"\x1b"[..], &b"[1"[..], &b";2"[..], &b"H"[..]] {
            p.feed_with(chunk, |e| seen.push(own(e)));
        }
        assert_eq!(
            seen,
            vec![Token::Csi {
                private: None,
                params: vec![1, 2],
                intermediates: vec![],
                final_byte: 'H'
            }]
        );
    }
}
