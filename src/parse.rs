//! Incremental VT/ANSI tokenizer.
//!
//! [`Parser::feed`] accepts a byte stream in chunks split at *any* boundary
//! (mid-escape-sequence, mid-UTF-8 character) and yields [`Token`]s. The
//! state machine follows the classic VT500-series design: C0 controls are
//! *executed* (emitted as [`Token::Control`]) even when they arrive inside an
//! escape sequence, `CAN`/`SUB` abort a sequence in progress, and a stray
//! `ESC` restarts one. Malformed sequences are dropped, never emitted as
//! text.

/// One parsed unit of terminal output.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    Str, // DCS / SOS / PM / APC
}

/// The incremental parser. Create one per stream and call
/// [`feed`](Parser::feed) with each chunk as it arrives.
#[derive(Debug, Default)]
pub struct Parser {
    state: Option<State>, // None == Ground via Default
    text: String,
    utf8: Vec<u8>,
    intermediates: Vec<u8>,
    param_bytes: Vec<u8>,
    private: Option<char>,
    str_intro: u8,
    str_data: Vec<u8>,
    str_esc: bool,
}

/// Cap accumulated sequence payloads so a hostile stream cannot grow memory
/// without bound; sequences exceeding this are dropped.
const MAX_SEQ: usize = 4096;

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> State {
        self.state.unwrap_or(State::Ground)
    }

    /// Feed a chunk of bytes; returns the tokens completed by this chunk.
    /// Any accumulated text run is flushed at the end of the call; partial
    /// escape sequences and partial UTF-8 characters are held for the next.
    pub fn feed(&mut self, input: &[u8]) -> Vec<Token> {
        let mut out = Vec::new();
        for &b in input {
            self.byte(b, &mut out);
        }
        self.flush_text(&mut out);
        out
    }

    fn flush_text(&mut self, out: &mut Vec<Token>) {
        if !self.text.is_empty() {
            out.push(Token::Text(std::mem::take(&mut self.text)));
        }
    }

    fn start(&mut self, state: State) {
        self.state = Some(state);
        self.intermediates.clear();
        self.param_bytes.clear();
        self.private = None;
        self.str_data.clear();
        self.str_esc = false;
    }

    fn byte(&mut self, b: u8, out: &mut Vec<Token>) {
        match self.state() {
            State::Ground => self.ground(b, out),
            State::Escape => self.escape(b, out),
            State::EscapeIntermediate => self.escape_intermediate(b, out),
            State::Csi => self.csi(b, out),
            State::Osc => self.string_byte(b, true, out),
            State::Str => self.string_byte(b, false, out),
        }
    }

    fn ground(&mut self, b: u8, out: &mut Vec<Token>) {
        if !self.utf8.is_empty() {
            if (0x80..0xC0).contains(&b) {
                self.utf8.push(b);
                let want = utf8_len(self.utf8[0]);
                if self.utf8.len() == want {
                    match std::str::from_utf8(&self.utf8) {
                        Ok(s) => self.text.push_str(s),
                        Err(_) => self.text.push('\u{FFFD}'),
                    }
                    self.utf8.clear();
                }
                return;
            }
            // Truncated sequence: the pending bytes were invalid.
            self.text.push('\u{FFFD}');
            self.utf8.clear();
            // fall through to process `b` normally
        }
        match b {
            0x1B => {
                self.flush_text(out);
                self.start(State::Escape);
            }
            0x00..=0x1F | 0x7F => {
                self.flush_text(out);
                out.push(Token::Control(b));
            }
            0x20..=0x7E => self.text.push(b as char),
            0x80..=0xBF => self.text.push('\u{FFFD}'), // stray continuation
            _ => {
                if utf8_len(b) == 1 {
                    self.text.push('\u{FFFD}'); // invalid lead (0xF8+)
                } else {
                    self.utf8.push(b);
                }
            }
        }
    }

    fn escape(&mut self, b: u8, out: &mut Vec<Token>) {
        match b {
            b'[' => self.start(State::Csi),
            b']' => self.start(State::Osc),
            b'P' | b'X' | b'^' | b'_' => {
                self.start(State::Str);
                self.str_intro = b;
            }
            0x20..=0x2F => {
                self.intermediates.push(b);
                self.state = Some(State::EscapeIntermediate);
            }
            0x30..=0x7E => {
                out.push(Token::Esc {
                    intermediates: Vec::new(),
                    final_byte: b,
                });
                self.state = None;
            }
            0x1B => self.start(State::Escape),
            0x18 | 0x1A => self.state = None, // CAN / SUB abort
            _ => out.push(Token::Control(b)), // execute C0 within sequence
        }
    }

    fn escape_intermediate(&mut self, b: u8, out: &mut Vec<Token>) {
        match b {
            0x20..=0x2F => {
                if self.intermediates.len() < 8 {
                    self.intermediates.push(b);
                }
            }
            0x30..=0x7E => {
                out.push(Token::Esc {
                    intermediates: std::mem::take(&mut self.intermediates),
                    final_byte: b,
                });
                self.state = None;
            }
            0x1B => self.start(State::Escape),
            0x18 | 0x1A => self.state = None,
            _ => out.push(Token::Control(b)),
        }
    }

    fn csi(&mut self, b: u8, out: &mut Vec<Token>) {
        match b {
            b'0'..=b'9' | b';' | b':' => {
                if self.param_bytes.len() < MAX_SEQ {
                    self.param_bytes.push(b);
                } else {
                    self.state = None; // oversized: drop the sequence
                }
            }
            0x3C..=0x3F => {
                if self.private.is_none() && self.param_bytes.is_empty() {
                    self.private = Some(b as char);
                } // later private markers are ignored, as most terminals do
            }
            0x20..=0x2F => {
                if self.intermediates.len() < 8 {
                    self.intermediates.push(b);
                }
            }
            0x40..=0x7E => {
                out.push(Token::Csi {
                    private: self.private.take(),
                    params: parse_params(&self.param_bytes),
                    intermediates: std::mem::take(&mut self.intermediates),
                    final_byte: b as char,
                });
                self.state = None;
            }
            0x1B => self.start(State::Escape),
            0x18 | 0x1A => self.state = None,
            _ => out.push(Token::Control(b)),
        }
    }

    fn string_byte(&mut self, b: u8, is_osc: bool, out: &mut Vec<Token>) {
        if self.str_esc {
            self.str_esc = false;
            if b == b'\\' {
                self.dispatch_string(is_osc, out);
                return;
            }
            if self.str_data.len() < MAX_SEQ {
                self.str_data.push(0x1B);
            }
            // fall through: `b` is ordinary payload
        }
        match b {
            0x07 => self.dispatch_string(is_osc, out), // BEL terminator
            0x1B => self.str_esc = true,
            _ => {
                if self.str_data.len() < MAX_SEQ {
                    self.str_data.push(b);
                }
            }
        }
    }

    fn dispatch_string(&mut self, is_osc: bool, out: &mut Vec<Token>) {
        let data = std::mem::take(&mut self.str_data);
        if is_osc {
            out.push(Token::Osc(String::from_utf8_lossy(&data).into_owned()));
        } else {
            out.push(Token::Other {
                intro: self.str_intro,
                data,
            });
        }
        self.state = None;
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

/// Split raw CSI parameter bytes on `;` into numbers. Empty parameters are
/// 0; digits stop at a `:` sub-parameter; values saturate at `u16::MAX`.
fn parse_params(bytes: &[u8]) -> Vec<u16> {
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes
        .split(|&b| b == b';')
        .take(32)
        .map(|seg| {
            let mut v: u32 = 0;
            for &b in seg {
                if !b.is_ascii_digit() {
                    break;
                }
                v = (v * 10 + (b - b'0') as u32).min(u16::MAX as u32);
            }
            v as u16
        })
        .collect()
}
