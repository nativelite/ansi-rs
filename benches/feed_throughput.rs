//! How fast the parser and the screen take terminal output: MiB/s over a
//! generated, agent-style byte stream.
//!
//! Run: `cargo bench --bench feed_throughput` (a release build). Under
//! `cargo test --all-targets` this target only builds and returns, so the
//! local gate stays fast.
//!
//! How it measures, so the numbers mean what they say:
//! - The input is deterministic: coloured test-runner lines (timestamp, pane
//!   tag, SGR-wrapped PASS/FAIL, a long test path, CRLF), with a cursor move
//!   and an erase-to-end-of-line every 50 lines and an OSC title every 500,
//!   `ANSI_BENCH_MB` MiB of it. Plain ASCII runs dominate, as agent output does.
//! - It is fed in 8 KiB chunks, the way a pty reader delivers output, so
//!   sequences and runs do split across calls.
//! - `feed_with`: `Parser::feed_with`, events counted and nothing else: the
//!   tokenizer alone, allocating nothing.
//! - `feed`: `Parser::feed`, the owned `Token`s: the same machine plus the
//!   `String`/`Vec` per token and UTF-8 validation of text.
//! - `screen ...`: `feed_with` driving a 177x47 `Screen` with the smallest
//!   interpreter that exercises the hot paths: text goes in with
//!   `Screen::write_ascii` (the stream is ASCII) and autowraps, LF at the
//!   bottom scrolls with `scroll_rows_up`, CR homes the column, `CSI m`
//!   applies SGR, `CSI H` moves the cursor; everything else is dropped. It is
//!   not a terminal emulator (`nativelite-vterm` is), so these rows measure
//!   this crate's share of emulation, not a terminal's. Three shapes: no
//!   history and full-screen scroll, a 10,000-line history, and a 10,000-line
//!   history with a one-row status line (`DECSTBM 1;46`, a scroll region).
//! - Each row is the median of `ANSI_BENCH_RUNS` runs; the spread is shown.
//!
//! Knobs (environment):
//! - `ANSI_BENCH_MB` (input size in MiB, default 64)
//! - `ANSI_BENCH_RUNS` (repeats per row, median reported, default 5)
//! - `ANSI_BENCH_ONLY` (`parse` or `screen`)

use std::time::Instant;

use ansi::{Cell, Cursor, Event, Parser, Screen, Style};

const ROWS: usize = 47;
const COLS: usize = 177;
const HISTORY: usize = 10_000;
const CHUNK: usize = 8192;

fn main() {
    if !std::env::args().any(|a| a == "--bench") {
        println!("feed_throughput bench: skipped (run `cargo bench --bench feed_throughput`)");
        return;
    }
    let mb = env_num("ANSI_BENCH_MB", 64);
    let runs = env_num("ANSI_BENCH_RUNS", 5);
    let only = std::env::var("ANSI_BENCH_ONLY").ok();
    let wants = |what: &str| only.as_deref().map_or(true, |o| o == what);

    println!("# nativelite-ansi feed throughput\n");
    println!(
        "- nativelite-ansi {} ({}), {} logical cores, {}",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::thread::available_parallelism().map_or(0, |n| n.get()),
        std::env::consts::OS
    );
    let input = agent_output(mb);
    let lines = input.iter().filter(|&&b| b == b'\n').count();
    println!(
        "- {} MiB of agent-style output ({lines} lines, {} B/line), fed in {CHUNK} B chunks; \
         median of {runs} runs\n",
        mb,
        input.len() / lines.max(1)
    );

    println!("| path | MiB/s (median) | min | max |");
    println!("| --- | --- | --- | --- |");
    if wants("parse") {
        report("Parser::feed_with", &input, runs, || {
            let mut p = Parser::new();
            let mut events = 0usize;
            for chunk in input.chunks(CHUNK) {
                p.feed_with(chunk, |_| events += 1);
            }
            events
        });
        report("Parser::feed", &input, runs, || {
            let mut p = Parser::new();
            let mut tokens = 0usize;
            for chunk in input.chunks(CHUNK) {
                tokens += p.feed(chunk).len();
            }
            tokens
        });
    }
    if wants("screen") {
        report(
            &format!("screen {ROWS}x{COLS}, no history, full-screen scroll"),
            &input,
            runs,
            || drive(&input, Screen::new(ROWS, COLS), ROWS - 1),
        );
        report(
            &format!("screen {ROWS}x{COLS}, {HISTORY}-line history, full-screen scroll"),
            &input,
            runs,
            || drive(&input, Screen::with_history(ROWS, COLS, HISTORY), ROWS - 1),
        );
        report(
            &format!("screen {ROWS}x{COLS}, {HISTORY}-line history, region scroll (status line)"),
            &input,
            runs,
            || drive(&input, Screen::with_history(ROWS, COLS, HISTORY), ROWS - 2),
        );
    }
    println!();
    println!(
        "`screen` rows drive `Screen` through a minimal interpreter (text, CR, LF, SGR, CUP), \
         not a terminal emulator; see the file header. A region scroll never feeds the \
         history, so that row's history stays empty by design."
    );
}

/// Time `work` `runs` times over `input` and print one table row.
fn report<T>(label: &str, input: &[u8], runs: usize, mut work: impl FnMut() -> T) {
    let mib = input.len() as f64 / (1024.0 * 1024.0);
    let mut rates = Vec::with_capacity(runs);
    for _ in 0..runs {
        let start = Instant::now();
        let out = work();
        let secs = start.elapsed().as_secs_f64();
        std::hint::black_box(out);
        rates.push(mib / secs);
    }
    rates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    println!(
        "| {label} | {:.0} | {:.0} | {:.0} |",
        rates[rates.len() / 2],
        rates[0],
        rates[rates.len() - 1]
    );
}

/// Feed `input` through a parser into `screen` with the smallest interpreter
/// that exercises the hot paths. Lines scroll within rows `0..=bottom`:
/// `bottom == rows - 1` is a full-screen scroll (and feeds the history),
/// anything less is a scroll region as `DECSTBM` sets one. Returns the lines
/// scrolled, so the work cannot be optimized away.
fn drive(input: &[u8], mut screen: Screen, bottom: usize) -> usize {
    let mut p = Parser::new();
    let mut style = Style::default();
    let (mut row, mut col) = (0usize, 0usize);
    let mut scrolled = 0usize;
    for chunk in input.chunks(CHUNK) {
        p.feed_with(chunk, |event| match event {
            Event::Text(mut text) => {
                while !text.is_empty() {
                    if col >= COLS {
                        // Autowrap.
                        col = 0;
                        line_feed(&mut screen, &mut row, bottom, &mut scrolled);
                    }
                    let n = screen.write_ascii(row, col, text, style);
                    col += n;
                    text = &text[n..];
                }
            }
            Event::Control(b'\n') => line_feed(&mut screen, &mut row, bottom, &mut scrolled),
            Event::Control(b'\r') => col = 0,
            Event::Csi {
                params,
                final_byte: b'm',
                private: None,
                ..
            } => style.apply_sgr(params),
            Event::Csi {
                params,
                final_byte: b'H',
                private: None,
                ..
            } => {
                row = (params.first().copied().unwrap_or(1).max(1) as usize - 1).min(ROWS - 1);
                col = (params.get(1).copied().unwrap_or(1).max(1) as usize - 1).min(COLS - 1);
            }
            _ => {}
        });
    }
    screen.set_cursor(Cursor::new(row.min(ROWS - 1), col.min(COLS - 1)));
    scrolled
}

#[inline]
fn line_feed(screen: &mut Screen, row: &mut usize, bottom: usize, scrolled: &mut usize) {
    if *row >= bottom {
        screen.scroll_rows_up(0, bottom, 1, Cell::default());
        *scrolled += 1;
    } else {
        *row += 1;
    }
}

/// A deterministic, agent-style log: timestamps, pane tags, coloured
/// PASS/FAIL, long test paths, CRLF; a cursor move plus erase-to-end-of-line
/// every 50 lines and an OSC title every 500. `mb` MiB.
fn agent_output(mb: usize) -> Vec<u8> {
    use std::io::Write;
    let target = mb * 1024 * 1024;
    let mut out = Vec::with_capacity(target + 256);
    let mut i = 0u64;
    while out.len() < target {
        if i % 500 == 0 {
            write!(out, "\x1b]0;agent-{} running\x07", i % 8).expect("vec write");
        }
        if i % 50 == 0 {
            write!(out, "\x1b[{};1H\x1b[K", (i / 50) % 47 + 1).expect("vec write");
        }
        let (colour, verdict) = if i % 17 == 0 {
            ("31", "FAIL")
        } else {
            ("32", "PASS")
        };
        write!(
            out,
            "2026-10-18T12:{:02}:{:02}.{:03}Z [agent-{}] \x1b[{colour}m{verdict}\x1b[0m \
             tests::suite_{:03}::case_{:06}_handles_input_without_panicking ... ok ({} ms)\r\n",
            (i / 60_000) % 60,
            (i / 1000) % 60,
            i % 1000,
            i % 8,
            (i / 97) % 1000,
            i,
            (i * 7) % 250
        )
        .expect("vec write");
        i += 1;
    }
    out.truncate(target);
    out
}

fn env_num(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}
