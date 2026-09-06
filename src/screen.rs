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
fn emit_changes(prev: Option<&Screen>, next: &Screen, out: &mut String) {
    use std::fmt::Write;
    let mut style = Style::default();
    // Where the terminal cursor is right now, when known. Printing advances
    // it; at the right edge the position becomes ambiguous (pending wrap),
    // so it is dropped to force an explicit move next time.
    let mut at: Option<(usize, usize)> = None;
    for row in 0..next.rows {
        for col in 0..next.cols {
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
    }
    out.push_str(&style.transition_to(&Style::default()));
    let (r, c) = next.cursor;
    write!(out, "\x1b[{};{}H", r + 1, c + 1).unwrap();
}
