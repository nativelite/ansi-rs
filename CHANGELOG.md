# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2026-09-22

### Added
- **`Parser::feed_with`, tokenizing without allocation.** The same state
  machine as `feed`, but it hands a closure borrowed `Event`s: text runs point
  into the caller's buffer, sequence payloads into scratch the parser reuses.
  Nothing is allocated per token. Measured on foldwave's `fwbench parse`
  (agent-like output, one i5-12600K P-core, 64 MiB, 5 reps): **192.5 -> 876.3
  MiB/s, 4.6x**. `Event` is exported alongside `Token`.

### Changed
- **`feed` is now built on `feed_with`** instead of being a second copy of the
  state machine, so the two can no longer drift. Its behaviour is unchanged:
  the existing suite passes untouched, and a new property test feeds a corpus
  at *every* chunk split and compares the two paths token for token.
- **Sequence payloads no longer reallocate.** CSI parameters accumulate
  numerically into a fixed array instead of being collected as bytes and
  re-parsed at dispatch, intermediates likewise, and an OSC/DCS payload buffer
  is cleared and reused rather than taken.
- **`Utf8Decoder`,** the piece a `feed_with` consumer needs: it turns raw
  `Event::Text` bytes into `str` pieces, carries a character split across
  chunks, and renders invalid bytes as U+FFFD. An ASCII run is handed over
  borrowed, so it allocates nothing. `feed` uses it too, so both paths decode
  identically.

## [0.3.1] - 2026-09-15

Scrolling, which a terminal does on nearly every line of output, no longer
costs a copy of the screen.

### Added
- **`Screen::scroll_rows_up` / `scroll_rows_down`.** Scroll rows `top..=bottom`
  by `n` and fill what comes in — the operation an emulator needs for `IND`,
  `RI`, `SU`, `SD` and a newline at the bottom of a scroll region. Callers used
  to do it cell by cell.

### Changed
- **Scrolling the whole screen is `O(cols)`, not `O(rows x cols)`.** Rows are
  stored as a ring, so a full-screen scroll — the common case, output arriving
  at the bottom — advances an origin and clears the rows coming in instead of
  moving every cell. A smaller region is one block move per row. Measured by
  feeding 16 MB of coloured log output through `nativelite-vterm`: 11.0 → 66.0
  MB/s at 40x160, and 5.3 → 63.8 MB/s at 60x240, where the cost no longer
  grows with the grid.
- `Screen`'s `PartialEq` is hand-written rather than derived, so the ring's
  position stays invisible: two screens are equal when they *show* the same
  thing. A scrolled screen equals a freshly built one with the same contents,
  exactly as before.

## [0.3.0] - 2026-09-14

Breaking: the double-width model and the cursor are typed.

### Changed
- **`Cell.width` is a `CellWidth` enum** (`Continuation`, `Single`, `Wide`),
  not a `u8`: a width of 3 is no longer representable. `CellWidth::columns()`
  gives the 0/1/2 column advance. `Cell::new`/`wide`/`continuation` are
  unchanged.
- **`Screen.cursor` is private.** Read it with `Screen::cursor()` and move it
  with `Screen::set_cursor()`, both as a `Cursor { row, col }` — named fields
  instead of a transposable `(usize, usize)`. `set_cursor` clamps onto the
  grid, so the parked cursor is always a real cell.

### Fixed
- README no longer claims `Screen` has no double-width support (it has since
  0.2.0).

## [0.2.0] - 2026-09-12

### Added
- **Double-width cells.** `Cell` gains a `width` field (0 = continuation, 1, 2)
  with the constructors `Cell::new`, `Cell::wide` and `Cell::continuation`. A
  wide glyph is a lead cell plus a continuation cell; the diff emits the glyph
  once and advances the cursor by two.
- **`Screen::copy_cells`**, which writes a slice of cells into a row.
- **`Screen::clear`**, which resets every cell and the cursor in place without
  reallocating.

### Changed
- The diff emits one erase-to-end-of-line (`CSI K`) when a row's tail turns
  blank, instead of writing each blank cell. `render_full` is unchanged.

## [0.1.0] - 2026-08-28

### Added
- `Style` / `Color` (default, 256-indexed, RGB): `sgr()` absolute sequences,
  `transition_to()` minimal style-change sequences, and `apply_sgr()` to
  replay parsed SGR parameters onto a style.
- `Parser`: incremental VT/ANSI tokenizer accepting chunks split at any
  byte boundary (mid-escape, mid-UTF-8). Tokens: `Text`, `Control`, `Csi`
  (private marker, params, intermediates, final), `Esc`, `Osc` (BEL and ST
  terminators), and raw `Other` for DCS/SOS/PM/APC. VT500-style semantics:
  C0 executes inside sequences, `CAN`/`SUB` abort, stray `ESC` restarts,
  malformed sequences are dropped, invalid UTF-8 becomes U+FFFD; payload
  sizes are bounded against hostile streams.
- `Screen` / `Cell`: styled cell grid with `diff()` (cursor moves + minimal
  SGR transitions + changed chars only; empty output for equal frames;
  dimension mismatch falls back to full repaint) and `render_full()`.
- Test suite: byte-exact SGR goldens, build→parse→apply round trips across
  style pairs, tokenizer fixtures verified one-shot *and* byte-at-a-time,
  and every screen diff replayed through the parser onto an interpreted
  screen that must equal the target frame.
- Stdlib-only `dev.py` runner (`check`, `test`, `fmt`, `guard`) and the
  Cargo.toml zero-dependency guard.

Second crate in the nativelite **agent terminal** suite (see
`roadmap/agent-terminal-suite.md` in `nativelite/ops`).

[Unreleased]: https://github.com/nativelite/ansi-rs/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/nativelite/ansi-rs/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/nativelite/ansi-rs/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/nativelite/ansi-rs/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/nativelite/ansi-rs/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/nativelite/ansi-rs/releases/tag/v0.1.0
