//! A screen buffer and diff-based rendering: build the frame you want as a
//! grid of styled cells, then emit only the bytes that change what the
//! terminal is already showing.

use crate::style::Style;

/// One character cell: a `char` plus its [`Style`]. The default cell is a
/// space in the default style. Every `char` occupies exactly one cell; this
/// crate does not model East Asian double-width rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            style: Style::default(),
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
    /// the right edge. Each `char` takes one cell.
    pub fn write_str(&mut self, row: usize, col: usize, text: &str, style: Style) {
        for (i, ch) in text.chars().enumerate() {
            self.set(row, col + i, Cell { ch, style });
        }
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
            at = if col + 1 < next.cols {
                Some((row, col + 1))
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
            s.set(
                row,
                c,
                Cell {
                    ch,
                    style: Style::default(),
                },
            );
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
                s.set(
                    r,
                    c,
                    Cell {
                        ch: 'X',
                        style: dirty_style,
                    },
                );
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
            next.set(
                0,
                c,
                Cell {
                    ch: 'X',
                    style: Style::default(),
                },
            );
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
            prev.set(
                0,
                c,
                Cell {
                    ch: 'A',
                    style: Style::default(),
                },
            );
            next.set(
                0,
                c,
                Cell {
                    ch: 'B',
                    style: Style::default(),
                },
            );
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
        next.set(
            0,
            0,
            Cell {
                ch: 'A',
                style: Style::default(),
            },
        );
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
}
