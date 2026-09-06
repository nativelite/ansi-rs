# ansi-rs
**ANSI/VT terminal output as data**, built entirely on the Rust standard
library. **Zero dependencies.**

The TUI ecosystem's answer to "put styled text on a terminal" is a 30-crate
dependency tree. This crate is the pure-bytes core of that job: build escape
sequences, parse them, and diff screen frames, with no I/O at all: no raw
mode, no stdin/stdout, no PTY. Feed it bytes from anywhere; write its bytes
to anywhere.

## Philosophy

See [nativelite-philosophy](https://github.com/nativelite/nativelite-philosophy) for the broader engineering standards and attack surface reduction strategy behind all nativelite packages.

## Three capabilities, one concern

**Build styled output.** `Style` + `Color` (default / 256-indexed / RGB)
render to SGR sequences: absolutely, or as a minimal transition from a
previous style:

```rust
use ansi::{Color, Style};

let warn = Style { bold: true, fg: Color::Indexed(1), ..Style::default() };
assert_eq!(warn.sgr(), "\x1b[0;1;31m");

let plain = Style::default();
assert_eq!(plain.transition_to(&warn), "\x1b[0;1;31m");
```

**Parse a terminal byte stream, incrementally.** `Parser::feed` accepts
chunks split at *any* byte boundary (mid-escape, mid-UTF-8) and yields
tokens: text runs, C0 controls, CSI/ESC/OSC sequences, and raw DCS/APC
strings. The state machine follows the classic VT500-series design: C0
controls execute even inside sequences, `CAN`/`SUB` abort, a stray `ESC`
restarts, malformed sequences are dropped rather than leaked as text, and
invalid UTF-8 becomes U+FFFD without desyncing the stream.

```rust
let mut p = ansi::Parser::new();
for token in p.feed(b"\x1b[1;31mred\x1b[0m") {
    // Csi { params: [1, 31], final_byte: 'm', .. }, Text("red"), Csi { .. }
}
```

**Render by diff.** `Screen` is a grid of styled `Cell`s. `Screen::diff`
emits only the bytes that change what the terminal already shows: cursor
moves, minimal SGR transitions, changed characters, and `render_full`
repaints from scratch. Equal frames emit zero bytes.

```rust
use ansi::{Screen, Style};

let mut prev = Screen::new(24, 80);
let mut next = prev.clone();
next.write_str(0, 0, "hello", Style::default());
stdout_write(&prev.diff(&next)); // your I/O, not ours
```

## What's deliberately out of scope

- **I/O and raw mode**: the `rawterm` crate's concern. This crate never
  touches a file descriptor.
- **Full terminal emulation**: the parser tokenizes; it does not maintain
  scrollback, tabs, or modes. `apply_sgr` is provided because the renderer
  and any output-interpreter need it.
- **East Asian double-width**: every `char` is one cell in `Screen`.
- **Scroll-region diff tricks**: the diff is cell-precise but does not emit
  scroll commands.

## Correctness

SGR goldens are asserted byte-exact, then round-tripped: every built
sequence is fed back through the parser and re-applied with `apply_sgr`,
which must reproduce the original style, for a spread of styles and all
transitions between them. Tokenizer fixtures (xterm/ECMA-48 sequences,
private modes, both OSC terminators, aborts, mid-sequence C0, invalid
UTF-8) are each verified twice: fed one-shot and fed byte-at-a-time, which
must yield identical tokens. The screen renderer is verified end-to-end:
every diff is replayed through the parser onto an interpreted screen, and
the result must equal the target frame exactly, including a byte-exact
golden for the minimal-update case.

## Development

```bash
python dev.py check   # zero-dependency guard + cargo test (the pre-push gate)
python dev.py test    # cargo test
python dev.py fmt     # cargo fmt --check
python dev.py guard   # zero-dependency guard
```

`dev.py` is a stdlib-only runner, so `python dev.py check` is the same
one-command local gate used across every nativelite package. The guard fails
if `Cargo.toml` declares any dependency: runtime, build, or dev.
