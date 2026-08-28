# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-08-28

### Added
- `Style` / `Color` (default, 256-indexed, RGB): `sgr()` absolute sequences,
  `transition_to()` minimal style-change sequences, and `apply_sgr()` to
  replay parsed SGR parameters onto a style.
- `Parser` — incremental VT/ANSI tokenizer accepting chunks split at any
  byte boundary (mid-escape, mid-UTF-8). Tokens: `Text`, `Control`, `Csi`
  (private marker, params, intermediates, final), `Esc`, `Osc` (BEL and ST
  terminators), and raw `Other` for DCS/SOS/PM/APC. VT500-style semantics:
  C0 executes inside sequences, `CAN`/`SUB` abort, stray `ESC` restarts,
  malformed sequences are dropped, invalid UTF-8 becomes U+FFFD; payload
  sizes are bounded against hostile streams.
- `Screen` / `Cell` — styled cell grid with `diff()` (cursor moves + minimal
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

[Unreleased]: https://github.com/nativelite/ansi-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/nativelite/ansi-rs/releases/tag/v0.1.0
