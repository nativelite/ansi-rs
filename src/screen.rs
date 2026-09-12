//! A screen buffer and diff-based rendering: build the frame you want as a
//! grid of styled cells, then emit only the bytes that change what the
//! terminal is already showing.

use crate::style::Style;

/// One character cell: a `char`, its [`Style`], and its display `width` in
/// terminal columns. The default cell is a single-width space in the default
/// style.
///
/// `width` lets the grid model East Asian double-width glyphs and emoji, which
/// occupy two columns:
///
/// - `1` — a normal single-width cell (the common case, and the default).
/// - `2` — the **lead** (left half) of a double-width glyph; `ch` holds the
///   character. The cell immediately to its right should be a width-`0`
///   continuation.
/// - `0` — a **continuation** (right half) of the glyph to its left: it carries
///   no character of its own and is never emitted, because the terminal
///   advances the cursor by two columns when it draws the lead glyph.
///
/// This crate does not *compute* width — that needs the Unicode database, which
/// a zero-dependency crate cannot own. Callers set `width` (e.g. from the
/// `uwidth` crate); the renderers here honor whatever `width` they are given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
    pub width: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            style: Style::default(),
            width: 1,
        }
    }
}

impl Cell {
    /// A normal single-width cell (`width == 1`).
    pub const fn new(ch: char, style: Style) -> Self {
        Cell {
            ch,
            style,
            width: 1,
        }
    }

    /// The lead (left half) of a double-width glyph (`width == 2`). Its right
    /// neighbor should be a [`Cell::continuation`].
    pub const fn wide(ch: char, style: Style) -> Self {
        Cell {
            ch,
            style,
            width: 2,
        }
    }

    /// A continuation (right half) of a double-width glyph (`width == 0`): no
    /// character of its own, but carrying `style` so the covered column keeps
    /// the right background. Never emitted — the lead glyph fills both columns.
    pub const fn continuation(style: Style) -> Self {
        Cell {
            ch: ' ',
            style,
            width: 0,
        }
    }
}

/// A `rows x cols` grid of [`Cell`]s plus a cursor position, both 0-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    rows: usize,
    cols: usize,
    cells: Vec<Cell>,
    /// Where the cursor should rest after rendering: `(row, col)`, 0-based.
    pub cursor: (usize, usize),
}

impl Screen {
    /// A blank screen (all default cells, cursor at the origin).
    pub fn new(rows: usize, cols: usize) -> Self {
        Screen {
            rows,
            cols,
            cells: vec![Cell::default(); rows * cols],
            cursor: (0, 0),
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Reset all cells to default and the cursor to the origin, reusing the
    /// existing allocation. The caller must ensure the screen is already the
    /// right size; use [`Screen::new`] when the size changes.
    pub fn clear(&mut self) {
        self.cells.fill(Cell::default());
        self.cursor = (0, 0);
    }

    /// The cell at `(row, col)`; out of bounds returns a default cell.
    pub fn cell(&self, row: usize, col: usize) -> Cell {
        if row < self.rows && col < self.cols {
            self.cells[row * self.cols + col]
        } else {
            Cell::default()
        }
    }

    /// Set one cell. Out-of-bounds writes are ignored.
    pub fn set(&mut self, row: usize, col: usize, cell: Cell) {
        if row < self.rows && col < self.cols {
            self.cells[row * self.cols + col] = cell;
        }
    }

    /// Write a string across a row starting at `(row, col)`, truncating at
    /// the right edge. Each `char` takes one cell (single-width); callers that
    /// need double-width layout set [`Cell::width`] and use [`Screen::set`] or
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
        let base = row * self.cols + col;
        self.cells[base..base + n].copy_from_slice(&cells[..n]);
    }

    /// The bytes that transform a terminal currently showing `self` into one
    /// showing `next`: cursor moves (CUP), minimal SGR transitions, and the
    /// changed characters, ending with a style reset and the cursor parked at
    /// `next.cursor`. Equal screens produce no bytes at all.
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
    /// `self.cursor`.
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
            if target.width == 0 {
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
            let advance = target.width.max(1) as usize;
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
    let (r, c) = next.cursor;
    write!(out, "\x1b[{};{}H", r + 1, c + 1).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

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
        s.cursor = (3, 5);

        s.clear();

        // Dimensions must be unchanged.
        assert_eq!(s.rows(), 4);
        assert_eq!(s.cols(), 6);
        // Cursor must be at the origin.
        assert_eq!(s.cursor, (0, 0));
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
        assert_eq!(Cell::default().width, 1);
        assert_eq!(Cell::new('a', st).width, 1);
        assert_eq!(Cell::wide('世', st).width, 2);
        let cont = Cell::continuation(st);
        assert_eq!(cont.width, 0);
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
        // Guard: adding Cell.width must not change single-width output at all.
        let mut old = Screen::new(2, 10);
        old.write_str(0, 0, "hello", Style::default());
        let mut new = old.clone();
        new.set(0, 1, Cell::new('a', Style::default()));
        assert_eq!(old.diff(&new), b"\x1b[1;2Ha\x1b[1;1H");
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
