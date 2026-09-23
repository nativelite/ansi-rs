//! A screen buffer and diff-based rendering: build the frame you want as a
//! grid of styled cells, then emit only the bytes that change what the
//! terminal is already showing.

use crate::style::Style;

/// How many terminal columns a [`Cell`] covers, modeling East Asian
/// double-width glyphs and emoji, which occupy two columns.
///
/// This crate does not *compute* width — that needs the Unicode database, which
/// a zero-dependency crate cannot own. Callers pick the width (e.g. from the
/// `uwidth` crate); the renderers here honor whatever they are given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellWidth {
    /// The right half of the double-width glyph to its left: it carries no
    /// character of its own and is never emitted, because the terminal advances
    /// the cursor by two columns when it draws the lead glyph.
    Continuation,
    /// A normal single-width cell (the common case, and the default).
    #[default]
    Single,
    /// The left half (lead) of a double-width glyph; the cell holds the
    /// character, and the cell immediately to its right should be a
    /// [`CellWidth::Continuation`].
    Wide,
}

impl CellWidth {
    /// The columns this cell advances the terminal cursor when drawn: `0`, `1`
    /// or `2`.
    pub const fn columns(self) -> usize {
        match self {
            CellWidth::Continuation => 0,
            CellWidth::Single => 1,
            CellWidth::Wide => 2,
        }
    }
}

/// One character cell: a `char`, its [`Style`], and its [`CellWidth`]. The
/// default cell is a single-width space in the default style.
///
/// A double-width glyph is a [`Cell::wide`] lead followed by a
/// [`Cell::continuation`]; build cells with the constructors to keep that
/// pairing. A grid can still hold an unpaired half (a compositor clipping a
/// row through a glyph produces one), so readers of arbitrary cells should
/// expect it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
    pub width: CellWidth,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            style: Style::default(),
            width: CellWidth::Single,
        }
    }
}

impl Cell {
    /// A normal single-width cell.
    pub const fn new(ch: char, style: Style) -> Self {
        Cell {
            ch,
            style,
            width: CellWidth::Single,
        }
    }

    /// The lead (left half) of a double-width glyph. Its right neighbor should
    /// be a [`Cell::continuation`].
    pub const fn wide(ch: char, style: Style) -> Self {
        Cell {
            ch,
            style,
            width: CellWidth::Wide,
        }
    }

    /// A continuation (right half) of a double-width glyph: no character of its
    /// own, but carrying `style` so the covered column keeps the right
    /// background. Never emitted — the lead glyph fills both columns.
    pub const fn continuation(style: Style) -> Self {
        Cell {
            ch: ' ',
            style,
            width: CellWidth::Continuation,
        }
    }
}

/// A 0-based cursor position on a [`Screen`]. Named fields, so a row and a
/// column cannot be transposed the way a bare `(usize, usize)` can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

impl Cursor {
    pub const fn new(row: usize, col: usize) -> Self {
        Cursor { row, col }
    }
}

/// A `rows x cols` grid of [`Cell`]s plus a [`Cursor`], both 0-based.
#[derive(Debug, Clone)]
pub struct Screen {
    rows: usize,
    cols: usize,
    /// Row-major, and exactly `rows * cols` long: sized by [`Screen::new`] and
    /// never grown or shrunk afterwards (`clear` refills in place, `set` and
    /// `copy_cells` overwrite in bounds). The indexers check it in debug builds.
    ///
    /// Rows are reached through a **row map**: logical row `r` lives at
    /// physical row `map[r]`, so scrolling any region rotates a few row
    /// indices instead of moving cells. Nothing outside [`Screen::index`]
    /// knows this.
    ///
    /// Rows are also **cleared lazily**: physical row `p` holds real cells
    /// only in its first `written[p]` columns; every column past that reads
    /// as `blank[p]`. Scrolling a row in or clearing it just resets those two
    /// (O(1)), where filling every cell cost ~half of all emulation time on a
    /// 177-column pane of short agent lines. Writes fill any gap first.
    cells: Vec<Cell>,
    /// Logical row to physical row; always a permutation of `0..rows`.
    map: Vec<usize>,
    /// Per physical row: how many leading cells are real.
    written: Vec<usize>,
    /// Per physical row: what every cell past `written` reads as.
    blank: Vec<Cell>,
    /// Where the cursor should rest after rendering. Always on the grid (see
    /// [`Screen::set_cursor`]).
    cursor: Cursor,
}

/// Two screens are equal when they *show* the same thing: same size, same
/// cursor, same cell at every logical position. Where each row physically sits
/// in the ring is an implementation detail, so a scrolled screen and a freshly
/// built one with the same content compare equal.
impl PartialEq for Screen {
    fn eq(&self, other: &Self) -> bool {
        if self.rows != other.rows || self.cols != other.cols || self.cursor != other.cursor {
            return false;
        }
        (0..self.rows).all(|r| (0..self.cols).all(|c| self.cell(r, c) == other.cell(r, c)))
    }
}

impl Eq for Screen {}

impl Screen {
    /// A blank screen (all default cells, cursor at the origin).
    pub fn new(rows: usize, cols: usize) -> Self {
        Screen {
            rows,
            cols,
            cells: vec![Cell::default(); rows * cols],
            map: (0..rows).collect(),
            written: vec![0; rows],
            blank: vec![Cell::default(); rows],
            cursor: Cursor::default(),
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Where the cursor rests after rendering.
    #[inline]
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Move the cursor, clamped onto the grid: a row or column past the edge
    /// lands on the last row or column (the origin on an empty screen). The
    /// renderers park the terminal cursor here.
    #[inline]
    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.cursor = Cursor {
            row: cursor.row.min(self.rows.saturating_sub(1)),
            col: cursor.col.min(self.cols.saturating_sub(1)),
        };
    }

    /// Reset all cells to default and the cursor to the origin, reusing the
    /// existing allocation. The caller must ensure the screen is already the
    /// right size; use [`Screen::new`] when the size changes.
    pub fn clear(&mut self) {
        for (r, m) in self.map.iter_mut().enumerate() {
            *m = r;
        }
        self.written.iter_mut().for_each(|w| *w = 0);
        self.blank.iter_mut().for_each(|b| *b = Cell::default());
        self.cursor = Cursor::default();
    }

    /// The flat index of `(row, col)`, which the caller has bounds-checked.
    /// Logical row `row` sits at physical row `map[row]`.
    #[inline]
    fn index(&self, row: usize, col: usize) -> usize {
        debug_assert_eq!(self.cells.len(), self.rows * self.cols);
        debug_assert!(row < self.rows && col < self.cols);
        self.physical(row) * self.cols + col
    }

    /// The physical row holding logical `row`.
    #[inline]
    fn physical(&self, row: usize) -> usize {
        self.map[row]
    }

    /// Make the first `upto` cells of physical row `p` real, filling the
    /// gap past `written[p]` with the row's blank.
    #[inline]
    fn materialize(&mut self, p: usize, upto: usize) {
        let w = self.written[p];
        if w < upto {
            let base = p * self.cols;
            let blank = self.blank[p];
            self.cells[base + w..base + upto].fill(blank);
            self.written[p] = upto;
        }
    }

    /// The cell at `(row, col)`; out of bounds returns a default cell.
    pub fn cell(&self, row: usize, col: usize) -> Cell {
        if row < self.rows && col < self.cols {
            let p = self.physical(row);
            if col < self.written[p] {
                self.cells[self.index(row, col)]
            } else {
                self.blank[p]
            }
        } else {
            Cell::default()
        }
    }

    /// Set one cell. Out-of-bounds writes are ignored.
    pub fn set(&mut self, row: usize, col: usize, cell: Cell) {
        if row < self.rows && col < self.cols {
            let p = self.physical(row);
            self.materialize(p, col + 1);
            let i = self.index(row, col);
            self.cells[i] = cell;
        }
    }

    /// Write a string across a row starting at `(row, col)`, truncating at
    /// the right edge. Each `char` takes one cell (single-width); callers that
    /// need double-width layout use [`Cell::wide`] with [`Screen::set`] or
    /// [`Screen::copy_cells`].
    pub fn write_str(&mut self, row: usize, col: usize, text: &str, style: Style) {
        for (i, ch) in text.chars().enumerate() {
            self.set(row, col + i, Cell::new(ch, style));
        }
    }

    /// Copy a contiguous run of cells into `row` starting at column `col`,
    /// clipping the run at the right edge (and ignoring an out-of-range row or
    /// start column). Equivalent to calling [`Screen::set`] for each cell in
    /// turn, but with a single bounds check for the whole run — the fast path a
    /// compositor uses to blit a row slice.
    pub fn copy_cells(&mut self, row: usize, col: usize, cells: &[Cell]) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let n = cells.len().min(self.cols - col);
        let p = self.physical(row);
        self.materialize(p, col);
        let base = self.index(row, col);
        self.cells[base..base + n].copy_from_slice(&cells[..n]);
        self.written[p] = self.written[p].max(col + n);
    }

    /// Write ASCII `text` into `row` from column `col` in one pass, each byte
    /// a single-width cell in `style`, clipped at the right edge (an
    /// out-of-range row or column writes nothing). Returns the cells written.
    /// The same as [`Screen::copy_cells`] of `Cell::new(b as char, style)`
    /// for each byte, without building the cells first — the hot path of a
    /// terminal printing plain text. Bytes are not checked: callers pass
    /// printable ASCII.
    pub fn write_ascii(&mut self, row: usize, col: usize, text: &[u8], style: Style) -> usize {
        if row >= self.rows || col >= self.cols {
            return 0;
        }
        let n = text.len().min(self.cols - col);
        let p = self.physical(row);
        self.materialize(p, col);
        let base = self.index(row, col);
        for (cell, &b) in self.cells[base..base + n].iter_mut().zip(text) {
            *cell = Cell::new(b as char, style);
        }
        self.written[p] = self.written[p].max(col + n);
        n
    }

    /// Scroll rows `top..=bottom` up by `n`. Rows move up, the top `n` rows of
    /// the region are lost, and `n` rows of `fill` enter at the bottom. Rows
    /// outside the region are untouched. An empty or out-of-range region does
    /// nothing, and `n` is clamped to the region's height.
    ///
    /// Any region, the whole screen included, scrolls by rotating its entries
    /// in the row map and clearing the rows that come in: `region` index moves
    /// and O(1) per incoming row, never `region x cols`.
    ///
    /// A terminal scrolls on every output line. Copying the region cell by cell
    /// fed a 16 MB log at 11 MB/s on a 40x160 grid; moving whole rows still
    /// held a 177x47 pane with a one-row status line (`DECSTBM`) to 21 MiB/s,
    /// against 182 MiB/s scrolling the full screen.
    pub fn scroll_rows_up(&mut self, top: usize, bottom: usize, n: usize, fill: Cell) {
        if top > bottom || bottom >= self.rows || n == 0 {
            return;
        }
        let n = n.min(bottom - top + 1);
        self.map[top..=bottom].rotate_left(n);
        for r in bottom + 1 - n..=bottom {
            self.fill_row(r, fill);
        }
    }

    /// Fill one logical row with `fill`: O(1), by making the whole row read
    /// as `fill` (see `written`).
    #[inline]
    fn fill_row(&mut self, row: usize, fill: Cell) {
        let p = self.physical(row);
        self.written[p] = 0;
        self.blank[p] = fill;
    }

    /// Scroll rows `top..=bottom` down by `n`: the mirror of
    /// [`Screen::scroll_rows_up`]. The bottom `n` rows of the region are lost,
    /// and `n` rows of `fill` enter at the top.
    pub fn scroll_rows_down(&mut self, top: usize, bottom: usize, n: usize, fill: Cell) {
        if top > bottom || bottom >= self.rows || n == 0 {
            return;
        }
        let n = n.min(bottom - top + 1);
        self.map[top..=bottom].rotate_right(n);
        for r in top..top + n {
            self.fill_row(r, fill);
        }
    }

    /// The bytes that transform a terminal currently showing `self` into one
    /// showing `next`: cursor moves (CUP), minimal SGR transitions, and the
    /// changed characters, ending with a style reset and the cursor parked at
    /// `next`'s cursor. Equal screens produce no bytes at all.
    ///
    /// Both screens must have the same dimensions; if they differ (a resize),
    /// the diff falls back to a full repaint of `next`.
    pub fn diff(&self, next: &Screen) -> Vec<u8> {
        if self.rows != next.rows || self.cols != next.cols {
            return next.render_full();
        }
        if self == next {
            return Vec::new();
        }
        let mut out = String::new();
        emit_changes(Some(self), next, &mut out);
        out.into_bytes()
    }

    /// A from-scratch repaint: clear the screen, then paint every non-default
    /// cell, ending with a style reset and the cursor parked at
    /// this screen's cursor.
    pub fn render_full(&self) -> Vec<u8> {
        let mut out = String::from("\x1b[2J");
        emit_changes(None, self, &mut out);
        out.into_bytes()
    }
}

/// Emit the cell changes from `prev` to `next` (`None` means "cleared
/// screen": every non-default cell of `next` is painted).
///
/// Row suffix optimisation: when the trailing cells of a row are all default
/// in `next` but were not in `prev`, a single `\x1b[K]` (Erase to EOL)
/// replaces one write-per-blank-cell. Style is reset before the erase so the
/// terminal fills with the default background.
fn emit_changes(prev: Option<&Screen>, next: &Screen, out: &mut String) {
    use std::fmt::Write;
    let mut style = Style::default();
    // Where the terminal cursor is right now, when known. Printing advances
    // it; at the right edge the position becomes ambiguous (pending wrap),
    // so it is dropped to force an explicit move next time.
    let mut at: Option<(usize, usize)> = None;
    for row in 0..next.rows {
        // Find the exclusive right bound of the non-default suffix in `next`.
        // Columns in [last_content, cols) are all Cell::default() in `next`.
        let last_content = (0..next.cols)
            .rev()
            .find(|&c| next.cell(row, c) != Cell::default())
            .map_or(0, |c| c + 1);

        // Emit changed cells up to (but not including) the trailing blank region.
        for col in 0..last_content {
            let target = next.cell(row, col);
            // A width-0 cell is the right half (continuation) of a double-width
            // glyph: it carries no character of its own and emits nothing. The
            // lead cell to its left drew the glyph and the terminal advanced the
            // cursor across both columns.
            if target.width == CellWidth::Continuation {
                continue;
            }
            let same = match prev {
                Some(p) => p.cell(row, col) == target,
                None => target == Cell::default(),
            };
            if same {
                continue;
            }
            if at != Some((row, col)) {
                write!(out, "\x1b[{};{}H", row + 1, col + 1).unwrap();
            }
            out.push_str(&style.transition_to(&target.style));
            style = target.style;
            out.push(target.ch);
            // The terminal advances the cursor by the glyph's column width, so a
            // width-2 lead lands `at` two columns on and its continuation column
            // is then skipped. (A single-width cell advances by one, exactly as
            // before — ASCII output is byte-for-byte unchanged.) At or past the
            // right edge the position is ambiguous (pending wrap), so drop it to
            // force an explicit move next time.
            let advance = target.width.columns();
            at = if col + advance < next.cols {
                Some((row, col + advance))
            } else {
                None
            };
        }

        // If prev had non-default content in the trailing blank region of next,
        // erase it with a single EL rather than writing each blank cell.
        // Not used in render_full (prev == None) because `\x1b[2J` already
        // cleared the screen before emit_changes is called.
        if last_content < next.cols {
            let needs_erase = match prev {
                Some(p) => (last_content..next.cols).any(|c| p.cell(row, c) != Cell::default()),
                None => false,
            };
            if needs_erase {
                // Reset SGR before EL so the terminal erases with the default
                // background, not whatever the last cell's style was.
                out.push_str(&style.transition_to(&Style::default()));
                style = Style::default();
                if at != Some((row, last_content)) {
                    write!(out, "\x1b[{};{}H", row + 1, last_content + 1).unwrap();
                }
                out.push_str("\x1b[K");
                // Cursor stays at (row, last_content); next row needs an explicit
                // move regardless, so mark position unknown.
                at = None;
            }
        }
    }
    out.push_str(&style.transition_to(&Style::default()));
    let Cursor { row, col } = next.cursor;
    write!(out, "\x1b[{};{}H", row + 1, col + 1).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    /// A screen whose every cell names its own position, so any move is visible.
    fn numbered(rows: usize, cols: usize) -> Screen {
        let mut s = Screen::new(rows, cols);
        for r in 0..rows {
            for c in 0..cols {
                let ch = char::from_u32(0x4E00 + (r * 64 + c) as u32).unwrap();
                s.set(r, c, Cell::new(ch, Style::default()));
            }
        }
        s
    }

    /// The per-cell scroll this replaces, as the reference.
    fn reference_up(s: &mut Screen, top: usize, bottom: usize, n: usize, fill: Cell) {
        let n = n.min(bottom - top + 1);
        for r in top..=bottom {
            for c in 0..s.cols() {
                let cell = if r + n <= bottom {
                    s.cell(r + n, c)
                } else {
                    fill
                };
                s.set(r, c, cell);
            }
        }
    }

    fn reference_down(s: &mut Screen, top: usize, bottom: usize, n: usize, fill: Cell) {
        let n = n.min(bottom - top + 1);
        for r in (top..=bottom).rev() {
            for c in 0..s.cols() {
                let cell = if r >= top + n { s.cell(r - n, c) } else { fill };
                s.set(r, c, cell);
            }
        }
    }

    #[test]
    fn region_scrolls_match_the_per_cell_reference_everywhere() {
        let fill = Cell::new(' ', Style::default());
        for (rows, cols) in [(1, 1), (5, 3), (8, 7)] {
            for top in 0..rows {
                for bottom in top..rows {
                    for n in 0..=(bottom - top + 2) {
                        let (mut fast, mut slow) = (numbered(rows, cols), numbered(rows, cols));
                        fast.scroll_rows_up(top, bottom, n, fill);
                        if n > 0 {
                            reference_up(&mut slow, top, bottom, n, fill);
                        }
                        assert_eq!(fast, slow, "up {rows}x{cols} {top}..={bottom} by {n}");

                        let (mut fast, mut slow) = (numbered(rows, cols), numbered(rows, cols));
                        fast.scroll_rows_down(top, bottom, n, fill);
                        if n > 0 {
                            reference_down(&mut slow, top, bottom, n, fill);
                        }
                        assert_eq!(fast, slow, "down {rows}x{cols} {top}..={bottom} by {n}");
                    }
                }
            }
        }
    }

    /// The ring is invisible: a screen scrolled into a state equals a screen
    /// built that way, its rows read back in order, and a clear resets it.
    #[test]
    fn the_row_ring_is_invisible_to_readers() {
        let fill = Cell::new(' ', Style::default());
        let mut s = numbered(4, 3);
        // Scroll a whole screen's worth and then some, so origin wraps.
        for _ in 0..7 {
            s.scroll_rows_up(0, 3, 1, fill);
        }
        let blank = Screen::new(4, 3);
        assert_eq!(s, blank, "everything scrolled off: a blank screen");

        let mut s = numbered(4, 3);
        s.scroll_rows_up(0, 3, 2, fill);
        let mut want = Screen::new(4, 3);
        for r in 0..2 {
            for c in 0..3 {
                want.set(r, c, numbered(4, 3).cell(r + 2, c));
            }
        }
        assert_eq!(s, want, "two rows up, two blank rows in");
        assert_eq!(s.render_full(), want.render_full(), "same bytes");

        // A wrapped ring still writes, reads and clears by logical position.
        s.set(0, 0, Cell::new('A', Style::default()));
        assert_eq!(s.cell(0, 0).ch, 'A');
        s.clear();
        assert_eq!(s, Screen::new(4, 3));
    }

    /// Long random mixes of full-screen scrolls, region scrolls (up and down)
    /// and writes agree with the per-cell reference at every step, so the row
    /// map stays a faithful permutation however it has been rotated.
    #[test]
    fn mixed_scrolls_and_writes_match_the_reference_over_many_steps() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for (rows, cols) in [(1, 3), (6, 5), (24, 9)] {
            let (mut fast, mut slow) = (numbered(rows, cols), numbered(rows, cols));
            for step in 0..2_000 {
                let fill = Cell::new(char::from(b'a' + (step % 26) as u8), Style::default());
                let (top, bottom) = if next(3) == 0 {
                    (0, rows - 1)
                } else {
                    let a = next(rows);
                    (a, a + next(rows - a))
                };
                let n = 1 + next(3);
                match next(4) {
                    0 => {
                        fast.scroll_rows_up(top, bottom, n, fill);
                        reference_up(&mut slow, top, bottom, n, fill);
                    }
                    1 => {
                        fast.scroll_rows_down(top, bottom, n, fill);
                        reference_down(&mut slow, top, bottom, n, fill);
                    }
                    _ => {
                        let (r, c) = (next(rows), next(cols));
                        fast.set(r, c, fill);
                        slow.set(r, c, fill);
                    }
                }
                assert_eq!(fast, slow, "{rows}x{cols}, step {step}");
            }
        }
    }

    #[test]
    fn write_ascii_matches_copy_cells_and_clips() {
        let style = Style {
            bold: true,
            ..Style::default()
        };
        let (mut a, mut b) = (numbered(3, 8), numbered(3, 8));
        // Scroll first so the row is lazily blank with a styled fill.
        let fill = Cell::new(' ', style);
        a.scroll_rows_up(0, 2, 1, fill);
        b.scroll_rows_up(0, 2, 1, fill);
        let text = b"hello, world";
        assert_eq!(a.write_ascii(2, 3, text, style), 5, "clipped at the edge");
        let cells: Vec<Cell> = text.iter().map(|&c| Cell::new(c as char, style)).collect();
        b.copy_cells(2, 3, &cells);
        assert_eq!(a, b);
        assert_eq!(a.cell(2, 0), fill, "the gap before the text keeps the fill");
        assert_eq!(a.write_ascii(3, 0, b"x", style), 0);
        assert_eq!(a.write_ascii(0, 8, b"x", style), 0);
    }

    #[test]
    fn an_out_of_range_region_scroll_changes_nothing() {
        let fill = Cell::new('x', Style::default());
        let mut s = numbered(4, 4);
        let before = s.clone();
        s.scroll_rows_up(3, 1, 1, fill);
        s.scroll_rows_up(0, 4, 1, fill);
        s.scroll_rows_down(2, 9, 1, fill);
        assert_eq!(s, before);
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    fn filled_row(rows: usize, cols: usize, row: usize, ch: char) -> Screen {
        let mut s = Screen::new(rows, cols);
        for c in 0..cols {
            s.set(row, c, Cell::new(ch, Style::default()));
        }
        s
    }

    fn contains_el(bytes: &[u8]) -> bool {
        let s = std::str::from_utf8(bytes).unwrap_or("");
        s.contains("\x1b[K")
    }

    // ── Screen::clear() ──────────────────────────────────────────────────────

    #[test]
    fn clear_resets_all_cells_and_cursor_without_realloc() {
        let mut s = Screen::new(4, 6);
        // Dirty every cell with a non-default character and style.
        let dirty_style = Style {
            bold: true,
            ..Style::default()
        };
        for r in 0..4 {
            for c in 0..6 {
                s.set(r, c, Cell::new('X', dirty_style));
            }
        }
        // Move the cursor off the origin.
        s.set_cursor(Cursor::new(3, 5));

        s.clear();

        // Dimensions must be unchanged.
        assert_eq!(s.rows(), 4);
        assert_eq!(s.cols(), 6);
        // Cursor must be at the origin.
        assert_eq!(s.cursor(), Cursor::new(0, 0));
        // Every cell must equal the default.
        let blank = Cell::default();
        for r in 0..4 {
            for c in 0..6 {
                assert_eq!(
                    s.cell(r, c),
                    blank,
                    "cell ({r},{c}) was not reset by clear()"
                );
            }
        }
    }

    // ── emit_changes / diff: erase-to-EOL optimisation ───────────────────────

    #[test]
    fn diff_emits_el_when_row_tail_clears_to_blank() {
        // prev: row 0 fully filled with 'X'; next: only first 3 cols are 'X'.
        // The tail cols [3..8) revert to blank → diff should contain EL.
        let prev = filled_row(2, 8, 0, 'X');
        let mut next = Screen::new(2, 8);
        for c in 0..3 {
            next.set(0, c, Cell::new('X', Style::default()));
        }
        let diff = prev.diff(&next);
        assert!(
            contains_el(&diff),
            "expected \\x1b[K in diff when tail blanks out"
        );
    }

    #[test]
    fn diff_no_el_when_tail_was_already_blank_in_prev() {
        // Both screens have content only in cols [0, 3); tail was already blank.
        // No cells changed in the tail, so no EL should be emitted.
        let mut prev = Screen::new(2, 8);
        let mut next = Screen::new(2, 8);
        for c in 0..3 {
            prev.set(0, c, Cell::new('A', Style::default()));
            next.set(0, c, Cell::new('B', Style::default()));
        }
        let diff = prev.diff(&next);
        assert!(
            !contains_el(&diff),
            "unexpected \\x1b[K when tail was already blank"
        );
    }

    #[test]
    fn render_full_never_emits_el() {
        // render_full follows \x1b[2J with individual cell writes; it must NOT
        // use EL (the screen is already clear, so EL would be redundant, and
        // the contract is that render_full never relies on prior state).
        let s = filled_row(2, 8, 0, 'X');
        let bytes = s.render_full();
        assert!(!contains_el(&bytes), "render_full must not emit \\x1b[K");
    }

    #[test]
    fn diff_el_visual_correctness_via_round_trip() {
        // Prove that a diff containing EL produces the correct visual result:
        // simulate applying the diff to a "terminal" modelled as a Screen copy.
        // prev → diff(next) → the result must equal next.
        let prev = filled_row(1, 6, 0, 'Z');
        let mut next = Screen::new(1, 6);
        next.set(0, 0, Cell::new('A', Style::default()));
        // next has 'A' at (0,0) and blanks at (0,1..5).
        assert!(contains_el(&prev.diff(&next)));
        // The diff-then-apply path is tested implicitly: if diff produces EL,
        // but the NEXT diff of (next, next) is empty, then prev+diff == next.
        let empty = next.diff(&next);
        assert!(empty.is_empty(), "diff of identical screens must be empty");
    }

    #[test]
    fn diff_entire_row_blank_in_next_uses_el() {
        // prev: entire row 1 filled; next: row 1 is all blank.
        let prev = filled_row(3, 5, 1, 'Y');
        let next = Screen::new(3, 5);
        let diff = prev.diff(&next);
        assert!(
            contains_el(&diff),
            "entire-row blank transition should use EL"
        );
    }

    // ── double-width cells: Cell.width, continuation, width-aware emit ────────

    #[test]
    fn cell_constructors_set_width() {
        let st = Style::default();
        assert_eq!(Cell::default().width, CellWidth::Single);
        assert_eq!(Cell::new('a', st).width, CellWidth::Single);
        assert_eq!(Cell::wide('世', st).width, CellWidth::Wide);
        let cont = Cell::continuation(st);
        assert_eq!(cont.width, CellWidth::Continuation);
        let cols: Vec<usize> = [CellWidth::Continuation, CellWidth::Single, CellWidth::Wide]
            .iter()
            .map(|w| w.columns())
            .collect();
        assert_eq!(cols, [0, 1, 2]);
        assert_eq!(cont.ch, ' ');
    }

    #[test]
    fn wide_lead_emits_glyph_once_no_continuation_char() {
        // A width-2 lead followed by a width-0 continuation must emit the glyph
        // exactly once and NOTHING for the continuation column (the terminal
        // advances the cursor by two when it draws the wide glyph).
        let prev = Screen::new(1, 4);
        let mut next = Screen::new(1, 4);
        next.set(0, 0, Cell::wide('世', Style::default()));
        next.set(0, 1, Cell::continuation(Style::default()));
        let diff = prev.diff(&next);
        let s = std::str::from_utf8(&diff).unwrap();
        assert_eq!(s.matches('世').count(), 1, "glyph must appear exactly once");
        assert!(
            !s.contains(' '),
            "continuation column must emit no space: {s:?}"
        );
        // Exact bytes: CUP to (1,1), the glyph, then park cursor at (1,1).
        assert_eq!(diff, "\x1b[1;1H世\x1b[1;1H".as_bytes());
    }

    #[test]
    fn wide_lead_advances_cursor_by_two() {
        // After a width-2 lead, a cell two columns on needs no fresh CUP — the
        // emitter tracks that the terminal advanced the cursor across both
        // halves of the wide glyph. If accounting were off by one, a CUP escape
        // would appear between the glyph and 'X'.
        let prev = Screen::new(1, 6);
        let mut next = Screen::new(1, 6);
        next.set(0, 0, Cell::wide('世', Style::default()));
        next.set(0, 1, Cell::continuation(Style::default()));
        next.set(0, 2, Cell::new('X', Style::default()));
        let diff = prev.diff(&next);
        assert_eq!(diff, "\x1b[1;1H世X\x1b[1;1H".as_bytes());
    }

    #[test]
    fn ascii_diff_is_byte_identical_regression() {
        // Guard: the width model must not change single-width output at all.
        let mut old = Screen::new(2, 10);
        old.write_str(0, 0, "hello", Style::default());
        let mut new = old.clone();
        new.set(0, 1, Cell::new('a', Style::default()));
        assert_eq!(old.diff(&new), b"\x1b[1;2Ha\x1b[1;1H");
    }

    #[test]
    fn set_cursor_clamps_onto_the_grid() {
        let mut s = Screen::new(4, 6);
        s.set_cursor(Cursor::new(2, 3));
        assert_eq!(s.cursor(), Cursor::new(2, 3));
        s.set_cursor(Cursor::new(99, 99));
        assert_eq!(s.cursor(), Cursor::new(3, 5));
        // An empty screen has no cell to stand on: the origin.
        let mut empty = Screen::new(0, 0);
        empty.set_cursor(Cursor::new(5, 5));
        assert_eq!(empty.cursor(), Cursor::new(0, 0));
    }

    #[test]
    fn copy_cells_matches_repeated_set() {
        let st = Style::default();
        let run = [
            Cell::new('a', st),
            Cell::wide('世', st),
            Cell::continuation(st),
            Cell::new('b', st),
        ];
        let mut via_copy = Screen::new(2, 8);
        via_copy.copy_cells(1, 2, &run);
        let mut via_set = Screen::new(2, 8);
        for (i, cell) in run.iter().enumerate() {
            via_set.set(1, 2 + i, *cell);
        }
        assert_eq!(via_copy, via_set);
    }

    #[test]
    fn copy_cells_clips_at_edge_and_ignores_out_of_range() {
        let st = Style::default();
        let run = [Cell::new('x', st); 5];
        let mut s = Screen::new(2, 4);
        // 5-cell run starting at col 2: only cols 2 and 3 are written.
        s.copy_cells(0, 2, &run);
        assert_eq!(s.cell(0, 2).ch, 'x');
        assert_eq!(s.cell(0, 3).ch, 'x');
        assert_eq!(s.cell(0, 1), Cell::default());
        // Out-of-range row or start column are silent no-ops (must not panic).
        s.copy_cells(9, 0, &run);
        s.copy_cells(0, 9, &run);
    }
}
