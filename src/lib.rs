//! ansi: ANSI/VT terminal output as data, on the Rust standard library
//! alone. Zero dependencies.
//!
//! Three tightly-related capabilities, all pure bytes in / bytes out:
//!
//! * **Build** styled output: [`Style`] + [`Color`] render to SGR escape
//!   sequences, absolutely ([`Style::sgr`]) or as a minimal transition from a
//!   previous style ([`Style::transition_to`]).
//! * **Parse** a terminal byte stream incrementally: [`Parser::feed`] accepts
//!   chunks split at *any* byte boundary (mid-escape, mid-UTF-8) and yields
//!   [`Token`]s (text runs, C0 controls, CSI/ESC/OSC sequences).
//! * **Render by diff**: [`Screen`] is a grid of styled [`Cell`]s;
//!   [`Screen::diff`] emits the minimal cursor-move/SGR/text bytes that turn
//!   one screen into another, and [`Screen::render_full`] repaints from
//!   scratch.
//!
//! ```
//! use ansi::{Color, Style};
//!
//! let style = Style { bold: true, fg: Color::Indexed(2), ..Style::default() };
//! assert_eq!(style.sgr(), "\x1b[0;1;32m");
//!
//! let mut p = ansi::Parser::new();
//! let tokens = p.feed(b"hi\x1b[1;31mred");
//! assert_eq!(tokens.len(), 3); // Text("hi"), Csi(SGR 1;31), Text("red")
//! ```
//!
//! There is no I/O here: no raw mode, no stdin/stdout, no PTY. Those are the
//! `rawterm` and `pty` crates' concerns; this crate turns bytes into meaning
//! and meaning into bytes.

mod parse;
mod screen;
mod style;

pub use parse::{Parser, Token};
pub use screen::{Cell, Screen};
pub use style::{Color, Style};
