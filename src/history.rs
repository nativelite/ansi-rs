//! Scrollback kept compactly: each line as its characters in UTF-8 plus runs
//! of style, not a full [`Cell`] per column. A 177-column row of cells is
//! 3.5 KB; the same row holding a 50-character line encodes to ~90 bytes.
//! Lines are appended to fixed-size chunks, so a scroll writes a few dozen
//! sequential bytes instead of touching a cold row of a large ring.

use crate::screen::{Cell, CellWidth};
use crate::style::{Color, Style};
use std::collections::VecDeque;

/// Bytes per chunk. A line longer than this gets a chunk of its own.
const CHUNK: usize = 64 * 1024;

/// Emptied chunks kept for reuse rather than freed.
const KEEP_FREE: usize = 2;

/// Lines, back to back: `data[starts[i]..starts[i + 1]]` is line
/// `first + i` (the last runs to the end of `data`).
#[derive(Debug, Clone, Default)]
struct Chunk {
    first: u64,
    starts: Vec<u32>,
    data: Vec<u8>,
}

impl Chunk {
    fn end(&self) -> u64 {
        self.first + self.starts.len() as u64
    }

    /// Memory it holds: what its buffers have allocated, not just used.
    fn held(&self) -> usize {
        self.data.capacity() + self.starts.capacity() * std::mem::size_of::<u32>()
    }

    fn line(&self, n: u64) -> &[u8] {
        let i = (n - self.first) as usize;
        let from = self.starts[i] as usize;
        let to = self
            .starts
            .get(i + 1)
            .map_or(self.data.len(), |&s| s as usize);
        &self.data[from..to]
    }
}

/// Up to `capacity` encoded lines, numbered from 0 as they arrive; the
/// oldest are dropped past capacity, and, with a byte cap
/// ([`History::set_max_bytes`]), whole oldest chunks while the encoded lines
/// take more than that.
#[derive(Debug, Clone, Default)]
pub(crate) struct History {
    chunks: VecDeque<Chunk>,
    free: Vec<Chunk>,
    /// The oldest line held.
    head: u64,
    /// The number the next line will get: every line ever pushed.
    next: u64,
    capacity: usize,
    /// Scratch for [`encode`], kept to avoid an allocation per line.
    runs: Vec<(u32, Style)>,
    odd: Vec<(u32, CellWidth)>,
    /// The last style a short line used, encoded.
    style: Option<(Style, [u8; STYLE])>,
    /// Per line held, oldest first: whether it wrapped onto the next line
    /// (the screen's autowrap, not a line break of its own).
    wrapped: VecDeque<bool>,
    /// Memory `chunks` hold (their buffers' capacity), and the most they may
    /// (0: no cap).
    bytes: usize,
    max_bytes: usize,
}

impl History {
    pub(crate) fn new(capacity: usize) -> Self {
        History {
            capacity,
            ..History::default()
        }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn len(&self) -> usize {
        (self.next - self.head) as usize
    }

    /// Lines ever pushed, including ones since dropped.
    pub(crate) fn pushed(&self) -> u64 {
        self.next
    }

    /// Append a line: its first `cells`, the rest of the row reading as
    /// `blank`; `wrapped` if it continued onto the next line. Does nothing
    /// with no capacity.
    pub(crate) fn push(&mut self, cells: &[Cell], blank: Cell, wrapped: bool) {
        if self.begin(wrapped) {
            let chunk = self.chunks.back_mut().expect("a chunk to append to");
            let before = chunk.held();
            encode(&mut chunk.data, cells, blank, &mut self.runs, &mut self.odd);
            self.bytes += chunk.held() - before;
            self.trim();
        }
    }

    /// Append a line whose first cells are `text`, a single-width cell per
    /// byte (as [`crate::Screen::write_ascii`] writes them) in `style`, the
    /// rest reading as `blank`: [`History::push`] without looking at cells.
    pub(crate) fn push_ascii(&mut self, text: &[u8], style: Style, blank: Cell, wrapped: bool) {
        if self.begin(wrapped) {
            let chunk = self.chunks.back_mut().expect("a chunk to append to");
            let before = chunk.held();
            encode_ascii(&mut chunk.data, text, style, blank, &mut self.style);
            self.bytes += chunk.held() - before;
            self.trim();
        }
    }

    /// Hold at most `max` bytes of memory (0: no cap), dropping whole oldest
    /// chunks past it, so the history's memory is bounded however wide or
    /// colourful its lines are. The newest chunk always stays, so it may run
    /// over by up to a chunk (96 KiB) or one very long line.
    pub(crate) fn set_max_bytes(&mut self, max: usize) {
        self.max_bytes = max;
        self.trim();
    }

    /// Memory held by the lines (their chunks' buffers).
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Drop whole oldest chunks while over the byte cap.
    fn trim(&mut self) {
        while self.max_bytes > 0 && self.bytes > self.max_bytes && self.chunks.len() > 1 {
            let c = self.chunks.pop_front().expect("more than one chunk");
            let end = c.end();
            for _ in self.head..end {
                self.wrapped.pop_front();
            }
            self.head = end;
            self.bytes -= c.held();
            self.recycle(c);
        }
    }

    fn recycle(&mut self, mut c: Chunk) {
        if self.free.len() < KEEP_FREE {
            c.starts.clear();
            c.data.clear();
            self.free.push(c);
        }
    }

    /// Make room for one more line and start it in the last chunk; false,
    /// doing nothing, with no capacity.
    fn begin(&mut self, wrapped: bool) -> bool {
        if self.capacity == 0 {
            return false;
        }
        if self.len() == self.capacity {
            self.drop_oldest();
        }
        let full = self
            .chunks
            .back()
            .map_or(true, |c| c.data.len() >= CHUNK && !c.starts.is_empty());
        if full {
            let mut c = self.free.pop().unwrap_or_default();
            if c.data.capacity() == 0 {
                // Room for a line past the chunk size without doubling.
                c.data.reserve_exact(CHUNK + CHUNK / 2);
            }
            c.first = self.next;
            self.bytes += c.held();
            self.chunks.push_back(c);
        }
        let chunk = self.chunks.back_mut().expect("a chunk to append to");
        let before = chunk.held();
        chunk.starts.push(chunk.data.len() as u32);
        self.bytes += chunk.held() - before;
        self.wrapped.push_back(wrapped);
        self.next += 1;
        true
    }

    fn drop_oldest(&mut self) {
        self.head += 1;
        self.wrapped.pop_front();
        let spent = self.chunks.len() > 1 && self.chunks[0].end() <= self.head;
        if spent {
            let c = self.chunks.pop_front().expect("a spent chunk");
            self.bytes -= c.held();
            self.recycle(c);
        }
    }

    /// Forget every line; numbering continues.
    pub(crate) fn clear(&mut self) {
        while let Some(c) = self.chunks.pop_front() {
            self.recycle(c);
        }
        self.head = self.next;
        self.wrapped.clear();
        self.bytes = 0;
    }

    /// The encoded line `age` lines back (0 the newest), if held.
    fn line(&self, age: usize) -> Option<&[u8]> {
        if age >= self.len() {
            return None;
        }
        let n = self.next - 1 - age as u64;
        let i = self.chunks.partition_point(|c| c.end() <= n);
        Some(self.chunks[i].line(n))
    }

    /// Whether line `age` (0 the newest) wrapped onto the next; false if not
    /// held.
    pub(crate) fn wrapped(&self, age: usize) -> bool {
        age < self.wrapped.len() && self.wrapped[self.wrapped.len() - 1 - age]
    }

    /// Line `age` as `cols` cells into `out` (replacing its contents);
    /// false, leaving `out` empty, if the line is not held.
    pub(crate) fn row(&self, age: usize, cols: usize, out: &mut Vec<Cell>) -> bool {
        out.clear();
        match self.line(age) {
            Some(line) => {
                decode(line, cols, out);
                true
            }
            None => false,
        }
    }

    /// One cell of line `age`, if held (default past the line's end is the
    /// line's blank).
    pub(crate) fn cell(&self, age: usize, col: usize) -> Option<Cell> {
        self.line(age).map(|line| decode_cell(line, col))
    }
}

// Line encoding, integers little-endian. Text is every cell's char in
// UTF-8. A line is one of:
//   short (tag 0): one style, the default blank, under 65,536 cells -
//     u16 cells, style (9 bytes), text
//   full (tag 1): u32 cells, u32 text bytes, u32 runs, u32 odd widths;
//     blank: u32 char, style (9 bytes), u8 width; text; runs: u32 cells and
//     a style each, consecutive, covering every cell; odd widths: u32
//     column and u8 width for each cell not single-width
// Nearly every line is short, and a short one costs 12 bytes over its text.
const SHORT: u8 = 0;
const FULL: u8 = 1;
const STYLE: usize = 9;
const SHORT_HEADER: usize = 1 + 2 + STYLE;
const HEADER: usize = 1 + 16 + 4 + STYLE + 1;
const RUN: usize = 4 + STYLE;
const ODD: usize = 5;

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn color_bytes(c: Color) -> [u8; 4] {
    match c {
        Color::Default => [0, 0, 0, 0],
        Color::Indexed(i) => [1, i, 0, 0],
        Color::Rgb(r, g, b) => [2, r, g, b],
    }
}

fn color_at(b: &[u8], at: usize) -> Color {
    match b[at] {
        1 => Color::Indexed(b[at + 1]),
        2 => Color::Rgb(b[at + 1], b[at + 2], b[at + 3]),
        _ => Color::Default,
    }
}

fn style_bytes(s: Style) -> [u8; STYLE] {
    let (f, b) = (color_bytes(s.fg), color_bytes(s.bg));
    let flags = s.bold as u8
        | (s.dim as u8) << 1
        | (s.italic as u8) << 2
        | (s.underline as u8) << 3
        | (s.reverse as u8) << 4
        | (s.strike as u8) << 5;
    [f[0], f[1], f[2], f[3], b[0], b[1], b[2], b[3], flags]
}

fn style_at(b: &[u8], at: usize) -> Style {
    let f = b[at + 8];
    Style {
        fg: color_at(b, at),
        bg: color_at(b, at + 4),
        bold: f & 1 != 0,
        dim: f & 2 != 0,
        italic: f & 4 != 0,
        underline: f & 8 != 0,
        reverse: f & 16 != 0,
        strike: f & 32 != 0,
    }
}

fn width_code(w: CellWidth) -> u8 {
    match w {
        CellWidth::Continuation => 0,
        CellWidth::Single => 1,
        CellWidth::Wide => 2,
    }
}

fn width_of(code: u8) -> CellWidth {
    match code {
        0 => CellWidth::Continuation,
        2 => CellWidth::Wide,
        _ => CellWidth::Single,
    }
}

fn encode(
    out: &mut Vec<u8>,
    cells: &[Cell],
    blank: Cell,
    runs: &mut Vec<(u32, Style)>,
    odd: &mut Vec<(u32, CellWidth)>,
) {
    // Written cells equal to the blank add nothing at the end.
    let n = cells.len() - cells.iter().rev().take_while(|&&c| c == blank).count();
    let cells = &cells[..n];
    let at = out.len();
    out.resize(at + HEADER, 0);
    let mut b = [0u8; 4];
    for c in cells {
        if c.ch.is_ascii() {
            out.push(c.ch as u8);
        } else {
            out.extend_from_slice(c.ch.encode_utf8(&mut b).as_bytes());
        }
    }
    runs.clear();
    let mut rest = cells;
    while let Some(first) = rest.first() {
        let len = rest.iter().take_while(|c| c.style == first.style).count();
        runs.push((len as u32, first.style));
        rest = &rest[len..];
    }
    odd.clear();
    for (col, c) in cells.iter().enumerate() {
        if c.width != CellWidth::Single {
            odd.push((col as u32, c.width));
        }
    }
    finish(out, at, n, runs, odd, blank);
}

/// Append `text` as the characters of one cell per byte.
fn push_bytes(out: &mut Vec<u8>, text: &[u8]) {
    if text.is_ascii() {
        out.extend_from_slice(text);
    } else {
        // Each byte is a cell of that code point (U+0080..=U+00FF here).
        let mut b = [0u8; 4];
        for &c in text {
            out.extend_from_slice(char::from(c).encode_utf8(&mut b).as_bytes());
        }
    }
}

fn encode_ascii(
    out: &mut Vec<u8>,
    text: &[u8],
    style: Style,
    blank: Cell,
    last: &mut Option<(Style, [u8; STYLE])>,
) {
    let n = text.len();
    if blank == Cell::default() && n <= usize::from(u16::MAX) {
        let bytes = match *last {
            Some((s, b)) if s == style => b,
            _ => {
                let b = style_bytes(style);
                *last = Some((style, b));
                b
            }
        };
        let [lo, hi] = (n as u16).to_le_bytes();
        let [a, b, c, d, e, f, g, h, i] = bytes;
        out.reserve(SHORT_HEADER + n);
        out.extend_from_slice(&[SHORT, lo, hi, a, b, c, d, e, f, g, h, i]);
        push_bytes(out, text);
        return;
    }
    let at = out.len();
    out.resize(at + HEADER, 0);
    push_bytes(out, text);
    let run = [(n as u32, style)];
    let runs = if n == 0 { &run[..0] } else { &run[..] };
    finish(out, at, n, runs, &[], blank);
}

/// Complete the full line started at `at`, whose text is already appended
/// after room for the header: fill in the header, then add the runs and
/// widths.
fn finish(
    out: &mut Vec<u8>,
    at: usize,
    cells: usize,
    runs: &[(u32, Style)],
    odd: &[(u32, CellWidth)],
    blank: Cell,
) {
    let text = out.len() - at - HEADER;
    out[at] = FULL;
    let h = at + 1;
    out[h..h + 4].copy_from_slice(&(cells as u32).to_le_bytes());
    out[h + 4..h + 8].copy_from_slice(&(text as u32).to_le_bytes());
    out[h + 8..h + 12].copy_from_slice(&(runs.len() as u32).to_le_bytes());
    out[h + 12..h + 16].copy_from_slice(&(odd.len() as u32).to_le_bytes());
    out[h + 16..h + 20].copy_from_slice(&(blank.ch as u32).to_le_bytes());
    out[h + 20..h + 20 + STYLE].copy_from_slice(&style_bytes(blank.style));
    out[h + 20 + STYLE] = width_code(blank.width);
    for &(len, style) in runs {
        put_u32(out, len);
        out.extend_from_slice(&style_bytes(style));
    }
    for &(col, w) in odd {
        put_u32(out, col);
        out.push(width_code(w));
    }
}

/// The parts of an encoded line.
struct Parts<'a> {
    cells: usize,
    blank: Cell,
    text: &'a str,
    /// A short line's one style, or a full line's encoded runs.
    one: Option<Style>,
    runs: &'a [u8],
    odd: &'a [u8],
}

impl Parts<'_> {
    /// The style runs as (cells, style), covering every stored cell.
    fn runs(&self) -> impl Iterator<Item = (usize, Style)> + '_ {
        let one = self.one.filter(|_| self.cells > 0).map(|s| (self.cells, s));
        one.into_iter().chain(
            self.runs
                .chunks_exact(RUN)
                .map(|r| (u32_at(r, 0) as usize, style_at(r, 4))),
        )
    }
}

fn parts(b: &[u8]) -> Parts<'_> {
    // Written by the encoders from chars, so the text is always UTF-8.
    fn utf8(t: &[u8]) -> &str {
        std::str::from_utf8(t).unwrap_or("")
    }
    if b[0] == SHORT {
        return Parts {
            cells: usize::from(u16::from_le_bytes([b[1], b[2]])),
            blank: Cell::default(),
            text: utf8(&b[SHORT_HEADER..]),
            one: Some(style_at(b, 3)),
            runs: &[],
            odd: &[],
        };
    }
    let cells = u32_at(b, 1) as usize;
    let text = u32_at(b, 5) as usize;
    let runs = u32_at(b, 9) as usize;
    let odd = u32_at(b, 13) as usize;
    let blank = Cell {
        ch: char::from_u32(u32_at(b, 17)).unwrap_or(' '),
        style: style_at(b, 21),
        width: width_of(b[21 + STYLE]),
    };
    let t = HEADER;
    let r = t + text;
    let o = r + runs * RUN;
    Parts {
        cells,
        blank,
        text: utf8(&b[t..r]),
        one: None,
        runs: &b[r..o],
        odd: &b[o..o + odd * ODD],
    }
}

fn decode(b: &[u8], cols: usize, out: &mut Vec<Cell>) {
    let p = parts(b);
    let mut chars = p.text.chars();
    for (len, style) in p.runs() {
        for _ in 0..len {
            let ch = chars.next().unwrap_or(' ');
            out.push(Cell::new(ch, style));
        }
    }
    out.truncate(cols);
    for o in p.odd.chunks_exact(ODD) {
        if let Some(cell) = out.get_mut(u32_at(o, 0) as usize) {
            cell.width = width_of(o[4]);
        }
    }
    out.resize(cols.max(out.len()), p.blank);
}

fn decode_cell(b: &[u8], col: usize) -> Cell {
    let p = parts(b);
    if col >= p.cells {
        return p.blank;
    }
    let mut start = 0;
    let mut style = Style::default();
    for (len, s) in p.runs() {
        if col < start + len {
            style = s;
            break;
        }
        start += len;
    }
    let width = p
        .odd
        .chunks_exact(ODD)
        .find(|o| u32_at(o, 0) as usize == col)
        .map_or(CellWidth::Single, |o| width_of(o[4]));
    Cell {
        ch: p.text.chars().nth(col).unwrap_or(' '),
        style,
        width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(text: &str, style: Style) -> Vec<Cell> {
        text.chars().map(|c| Cell::new(c, style)).collect()
    }

    #[test]
    fn lines_decode_to_the_cells_they_were_encoded_from() {
        let red = Style {
            fg: Color::Indexed(1),
            bold: true,
            ..Style::default()
        };
        let rgb = Style {
            bg: Color::Rgb(1, 2, 3),
            italic: true,
            underline: true,
            reverse: true,
            strike: true,
            dim: true,
            ..Style::default()
        };
        let mut row = cells("ab", Style::default());
        row.extend(cells("é€", red));
        row.push(Cell::wide('漢', rgb));
        row.push(Cell::continuation(rgb));
        row.push(Cell::new('x', Style::default()));
        let blank = Cell::new(' ', rgb);
        let mut h = History::new(4);
        h.push(&row, blank, false);
        let mut out = Vec::new();
        assert!(h.row(0, 10, &mut out));
        let mut want = row.clone();
        want.resize(10, blank);
        assert_eq!(out, want);
        for (col, cell) in want.iter().enumerate() {
            assert_eq!(h.cell(0, col), Some(*cell), "column {col}");
        }
        // Narrower: cut; trailing written blanks are not stored.
        assert!(h.row(0, 3, &mut out));
        assert_eq!(out, want[..3]);
        h.push(&[blank, blank], blank, false);
        assert!(h.row(0, 2, &mut out));
        assert_eq!(out, [blank, blank]);
        assert!(!h.row(2, 2, &mut out) && out.is_empty());
        // Plain ASCII without cells, including a byte past ASCII.
        h.push_ascii(b"ok \xe9", red, blank, false);
        h.push_ascii(b"", red, blank, false);
        assert!(h.row(1, 5, &mut out));
        let mut want = cells("ok \u{e9}", red);
        want.push(blank);
        assert_eq!(out, want);
        assert_eq!(h.cell(1, 3), Some(Cell::new('\u{e9}', red)));
        assert!(h.row(0, 2, &mut out));
        assert_eq!(out, [blank, blank]);
        assert_eq!(h.cell(4, 0), None);
        // The short form: the default blank, styles changing between lines.
        h.push_ascii(b"abc", red, Cell::default(), false);
        h.push_ascii(b"de", rgb, Cell::default(), false);
        h.push_ascii(b"", rgb, Cell::default(), false);
        for (age, text, style) in [(2, "abc", red), (1, "de", rgb), (0, "", rgb)] {
            let mut want = cells(text, style);
            want.resize(4, Cell::default());
            assert!(h.row(age, 4, &mut out));
            assert_eq!(out, want, "age {age}");
            for (col, cell) in want.iter().enumerate() {
                assert_eq!(h.cell(age, col), Some(*cell), "age {age} col {col}");
            }
        }
    }

    #[test]
    fn wrap_flags_follow_their_lines_as_old_ones_drop() {
        let mut h = History::new(3);
        for (i, w) in [false, true, true, false].into_iter().enumerate() {
            h.push_ascii(
                format!("l{i}").as_bytes(),
                Style::default(),
                Cell::default(),
                w,
            );
        }
        // Held: l1 (wrapped), l2 (wrapped), l3; l0 dropped.
        assert_eq!(
            (h.wrapped(0), h.wrapped(1), h.wrapped(2)),
            (false, true, true)
        );
        assert!(!h.wrapped(3), "not held");
        h.clear();
        assert!(!h.wrapped(0));
        h.push(&[], Cell::default(), true);
        assert!(h.wrapped(0));
    }

    #[test]
    fn the_oldest_lines_drop_past_capacity_across_chunks() {
        let mut h = History::new(3000);
        let line = |i: usize| cells(&format!("{i:0>100}"), Style::default());
        for i in 0..10_000 {
            h.push(&line(i), Cell::default(), false);
        }
        assert_eq!((h.len(), h.pushed()), (3000, 10_000));
        let mut out = Vec::new();
        for age in [0, 1, 1234, 2999] {
            assert!(h.row(age, 100, &mut out));
            assert_eq!(out, line(9_999 - age), "age {age}");
        }
        // ~120 bytes a line: 3,000 lines in a handful of 64 KiB chunks.
        assert!(h.chunks.len() <= 7, "{} chunks", h.chunks.len());
        h.clear();
        assert_eq!((h.len(), h.pushed()), (0, 10_000));
        h.push(&line(7), Cell::default(), false);
        assert!(h.row(0, 100, &mut out));
        assert_eq!(out, line(7));
        assert_eq!(History::new(0).len(), 0);
    }

    #[test]
    fn a_byte_cap_drops_whole_old_chunks_and_keeps_the_newest_lines() {
        let mut h = History::new(1_000_000);
        h.set_max_bytes(256 * 1024);
        // Wide lines with a style change on every cell: a few KB each.
        let line = |i: usize| {
            let mut v = Vec::new();
            for c in 0..300usize {
                let style = Style {
                    fg: Color::Indexed((c % 200) as u8),
                    ..Style::default()
                };
                v.extend(cells(&format!("{}", (i + c) % 10), style));
            }
            v
        };
        for i in 0..5_000 {
            h.push(&line(i), Cell::default(), i % 2 == 1);
        }
        assert!(
            h.bytes() <= 256 * 1024 + 2 * CHUNK,
            "held {} bytes",
            h.bytes()
        );
        assert!(h.len() < 5_000 && h.len() > 20, "{} lines", h.len());
        assert_eq!(h.pushed(), 5_000);
        // The newest are intact, and their wrap marks with them.
        let mut out = Vec::new();
        for age in [0, 1, h.len() - 1] {
            assert!(h.row(age, 300, &mut out));
            assert_eq!(out, line(4_999 - age), "age {age}");
            assert_eq!(h.wrapped(age), (4_999 - age) % 2 == 1);
        }
        assert!(!h.row(h.len(), 300, &mut out), "older ones are gone");
        // Without a cap only the line count limits it.
        let mut free = History::new(1_000);
        for i in 0..1_000 {
            free.push(&line(i), Cell::default(), false);
        }
        assert_eq!(free.len(), 1_000);
        // Capping later trims at once.
        free.set_max_bytes(128 * 1024);
        assert!(free.bytes() <= 128 * 1024 + 2 * CHUNK && free.len() < 1_000);
        h.clear();
        assert_eq!(h.bytes(), 0);
    }
}
