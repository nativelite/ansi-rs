//! Shell-integration marks (OSC 133, and VS Code's OSC 633), found without a
//! full parse.
//!
//! Shells that support "semantic prompts" bracket every command with four
//! sequences: prompt start (`A`), command start (`B`, where the user's input
//! begins), output start (`C`, the command ran) and command end (`D`, with an
//! optional exit code), each `ESC ] 133 ; <letter> [; params] <BEL or ST>`.
//! A terminal that knows where they fall can jump by command rather than by
//! line, and show which commands failed.
//!
//! [`MarkScanner`] finds them in a raw byte stream, chunk by chunk, with no
//! other state: it does not need a parser or a screen, so it can run over
//! output nobody is looking at. A sequence split across chunks is found.

/// Which boundary a mark records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkKind {
    /// `A`: the prompt starts.
    PromptStart,
    /// `B`: the prompt ends and the command line begins.
    CommandStart,
    /// `C`: the command was submitted; its output follows.
    OutputStart,
    /// `D`: the command finished.
    CommandEnd,
}

impl MarkKind {
    /// The protocol letter.
    pub fn letter(self) -> u8 {
        match self {
            MarkKind::PromptStart => b'A',
            MarkKind::CommandStart => b'B',
            MarkKind::OutputStart => b'C',
            MarkKind::CommandEnd => b'D',
        }
    }

    fn from_letter(b: u8) -> Option<MarkKind> {
        match b {
            b'A' => Some(MarkKind::PromptStart),
            b'B' => Some(MarkKind::CommandStart),
            b'C' => Some(MarkKind::OutputStart),
            b'D' => Some(MarkKind::CommandEnd),
            _ => None,
        }
    }
}

/// One mark found in the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellMark {
    /// Stream offset of the `ESC` that opens the sequence.
    pub offset: u64,
    /// Which boundary.
    pub kind: MarkKind,
    /// For [`MarkKind::CommandEnd`], the exit code, when the shell sent one.
    pub exit_code: Option<i32>,
}

/// Longest sequence kept while waiting for its terminator; longer ones are
/// not marks we understand and are dropped.
const MAX_SEQ: usize = 128;

/// Finds shell-integration marks in a byte stream fed in chunks.
#[derive(Debug, Default, Clone)]
pub struct MarkScanner {
    /// Bytes of a candidate sequence, from its `ESC`; empty when none is open.
    pending: Vec<u8>,
    /// Stream offset of the candidate's `ESC`.
    start: u64,
    /// Stream offset of the next byte fed.
    pos: u64,
}

impl MarkScanner {
    /// A scanner at stream offset 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// A scanner that treats the next byte fed as stream offset `pos`.
    pub fn at(pos: u64) -> Self {
        MarkScanner {
            pos,
            ..Self::default()
        }
    }

    /// Stream offset of the next byte.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Scan `bytes`, appending every complete mark to `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<ShellMark>) {
        let mut i = 0;
        while i < bytes.len() {
            if self.pending.is_empty() {
                // Fast path: nothing open, skip to the next ESC.
                match bytes[i..].iter().position(|&b| b == 0x1B) {
                    None => {
                        self.pos += (bytes.len() - i) as u64;
                        return;
                    }
                    Some(p) => {
                        self.pos += p as u64;
                        i += p;
                        self.start = self.pos;
                        self.pending.push(0x1B);
                        self.pos += 1;
                        i += 1;
                    }
                }
                continue;
            }
            let b = bytes[i];
            self.step(b, out);
            self.pos += 1;
            i += 1;
        }
    }

    fn step(&mut self, b: u8, out: &mut Vec<ShellMark>) {
        let p = &self.pending;
        let n = p.len();
        // ESC \ ends a string: check before anything else.
        if n >= 2 && p[n - 1] == 0x1B {
            if b == b'\\' {
                self.pending.pop();
                self.finish(out);
            } else {
                self.restart_at_last_esc(b);
            }
            return;
        }
        match b {
            0x07 if n >= 6 => self.finish(out),
            0x1B if n >= 6 => self.pending.push(0x1B), // maybe ST
            0x1B => {
                // A fresh ESC restarts the candidate.
                self.pending.clear();
                self.pending.push(0x1B);
                self.start = self.pos;
            }
            _ => {
                let ok = match n {
                    1 => b == b']',
                    2 | 3 => b == b'1' || b == b'6' || b == b'3',
                    4 => b == b'3',
                    5 => b == b';',
                    6 => MarkKind::from_letter(b).is_some(),
                    _ => n < MAX_SEQ && b >= 0x20,
                };
                if ok {
                    self.pending.push(b);
                    if n == 4 && !matches!(&self.pending[2..5], b"133" | b"633") {
                        self.pending.clear();
                    }
                } else {
                    self.pending.clear();
                }
            }
        }
    }

    /// The candidate had an ESC that turned out not to start ST; treat that
    /// ESC as the start of a new candidate and feed `b` to it.
    fn restart_at_last_esc(&mut self, b: u8) {
        self.pending.clear();
        self.pending.push(0x1B);
        self.start = self.pos - 1;
        let mut sink = Vec::new();
        self.step(b, &mut sink);
    }

    fn finish(&mut self, out: &mut Vec<ShellMark>) {
        // pending: ESC ] 1 3 3 ; X [; params]
        let p = std::mem::take(&mut self.pending);
        if p.len() < 7 {
            return;
        }
        let Some(kind) = MarkKind::from_letter(p[6]) else {
            return;
        };
        let params = &p[7..];
        let exit_code = if kind == MarkKind::CommandEnd {
            params
                .strip_prefix(b";")
                .and_then(|rest| rest.split(|&c| c == b';').next())
                .and_then(|code| std::str::from_utf8(code).ok())
                .and_then(|code| code.parse::<i32>().ok())
        } else {
            None
        };
        out.push(ShellMark {
            offset: self.start,
            kind,
            exit_code,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(bytes: &[u8]) -> Vec<ShellMark> {
        let mut s = MarkScanner::new();
        let mut out = Vec::new();
        s.feed(bytes, &mut out);
        out
    }

    fn cycle() -> Vec<u8> {
        b"\x1b]133;A\x07$ \x1b]133;B\x07cargo test\r\n\x1b]133;C\x07running...\r\nFAILED\r\n\x1b]133;D;101\x1b\\".to_vec()
    }

    #[test]
    fn finds_a_whole_command_cycle() {
        let data = cycle();
        let marks = scan(&data);
        let kinds: Vec<_> = marks.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            [
                MarkKind::PromptStart,
                MarkKind::CommandStart,
                MarkKind::OutputStart,
                MarkKind::CommandEnd
            ]
        );
        assert_eq!(marks[3].exit_code, Some(101));
        for m in &marks {
            assert_eq!(data[m.offset as usize], 0x1B, "{m:?}");
            assert_eq!(data[m.offset as usize + 6], m.kind.letter());
        }
    }

    #[test]
    fn every_chunk_split_finds_the_same_marks() {
        let mut data = b"noise \x1b[1mbold\x1b[0m ".to_vec();
        data.extend(cycle());
        data.extend_from_slice(b"\x1b]633;D\x07\x1b]0;title\x07tail");
        let whole = scan(&data);
        assert_eq!(whole.len(), 5);
        for split in 0..=data.len() {
            let mut s = MarkScanner::new();
            let mut out = Vec::new();
            s.feed(&data[..split], &mut out);
            s.feed(&data[split..], &mut out);
            assert_eq!(out, whole, "split at {split}");
            assert_eq!(s.position(), data.len() as u64);
        }
        // One byte at a time, too.
        let mut s = MarkScanner::new();
        let mut out = Vec::new();
        for b in &data {
            s.feed(std::slice::from_ref(b), &mut out);
        }
        assert_eq!(out, whole);
    }

    #[test]
    fn exit_codes_are_optional_and_parsed_when_present() {
        let m = scan(b"\x1b]133;D\x07\x1b]133;D;0\x07\x1b]133;D;-1;aid=7\x07\x1b]133;D;x\x07");
        let codes: Vec<_> = m.iter().map(|m| m.exit_code).collect();
        assert_eq!(codes, [None, Some(0), Some(-1), None]);
    }

    #[test]
    fn look_alikes_are_not_marks() {
        let m = scan(b"\x1b]0;title\x07\x1b]1330;A\x07\x1b]133;Z\x07\x1b[133;A\x07\x1b]133A\x07");
        assert!(m.is_empty(), "{m:?}");
    }

    #[test]
    fn an_esc_inside_a_candidate_restarts_it() {
        let data = b"\x1b]133\x1b]133;A\x07";
        let m = scan(data);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].offset, 5);
    }

    #[test]
    fn offsets_continue_from_a_starting_position() {
        let mut s = MarkScanner::at(1000);
        let mut out = Vec::new();
        s.feed(b"xx\x1b]133;C\x07", &mut out);
        assert_eq!(out[0].offset, 1002);
    }
}
