//! What scrollback costs: the bytes a `Screen::with_history` holds after
//! 10,000 lines have scrolled off its top, against what the same lines cost
//! as a ring of cells (the layout before 0.4.1).
//!
//! Run: `cargo bench --bench history_memory` (a release build). Under
//! `cargo test --all-targets` this target only builds and returns, so the
//! local gate stays fast.
//!
//! How it measures:
//! - A 177x47 screen with a 10,000-line history, the changelog's shape: a
//!   wide pane in a tiling terminal showing short agent lines.
//! - Each line is written with `Screen::write_ascii` on the bottom row, then
//!   the whole screen scrolls up one row, which is what feeds the history.
//!   `rows` extra lines are written so that exactly 10,000 lines have left
//!   the screen.
//! - **Reported** is `Screen::history_bytes()`: the capacity of the history's
//!   buffers, the number the crate itself can answer for. The "ring of
//!   cells" figure is arithmetic, `(rows + history) * cols * size_of::<Cell>()`,
//!   which is what the pre-0.4.1 layout allocated up front.
//! - Also reported: the process's peak resident set before and after the
//!   fill (Windows and Linux only), as a rough cross-check that the heap
//!   agrees with `history_bytes`. It includes allocator slack, so read it as
//!   an order of magnitude.
//! - Three variants: one-style ASCII lines (the usual agent line, stored by
//!   copying bytes), lines with an SGR colour change mid-line (stored as
//!   text plus style runs), and full-width 177-character lines.
//!
//! Knobs (environment):
//! - `ANSI_BENCH_LINES` (history capacity and lines scrolled off, default 10000)
//! - `ANSI_BENCH_RUNS` (timing repeats, median reported, default 5)

use std::time::Instant;

use ansi::{Cell, Color, Screen, Style};

const ROWS: usize = 47;
const COLS: usize = 177;
const LINE_LEN: usize = 48;

fn main() {
    if !std::env::args().any(|a| a == "--bench") {
        println!("history_memory bench: skipped (run `cargo bench --bench history_memory`)");
        return;
    }
    let lines = env_num("ANSI_BENCH_LINES", 10_000);
    let runs = env_num("ANSI_BENCH_RUNS", 5);

    println!("# nativelite-ansi history memory\n");
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
    println!(
        "- screen {ROWS}x{COLS}, history {lines} lines, {lines} lines scrolled off; \
         Cell is {} bytes",
        std::mem::size_of::<Cell>()
    );
    let ring = (ROWS + lines) * COLS * std::mem::size_of::<Cell>();
    println!(
        "- a ring of `rows + history` rows of cells (the pre-0.4.1 layout) is {} \
         for this shape\n",
        human(ring)
    );

    println!("| lines | history_bytes | bytes/line | vs ring of cells | fill ms (median of {runs}) | peak RSS before -> after |");
    println!("| --- | --- | --- | --- | --- | --- |");
    for (label, kind) in [
        (format!("{LINE_LEN}-char, one style"), Kind::Plain),
        (
            format!("{LINE_LEN}-char, colour change mid-line"),
            Kind::Styled,
        ),
        (format!("{COLS}-char, one style"), Kind::Full),
    ] {
        let mut ms = Vec::with_capacity(runs);
        let mut bytes = 0;
        let mut held = 0;
        let rss_before = peak_rss();
        for _ in 0..runs {
            let start = Instant::now();
            let screen = fill(lines, kind);
            ms.push(start.elapsed().as_secs_f64() * 1000.0);
            bytes = screen.history_bytes();
            held = screen.history_len();
            assert_eq!(held, lines, "every line that left the top is held");
        }
        let rss_after = peak_rss();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        println!(
            "| {label} | {} | {:.1} | {:.0}x smaller | {:.1} | {} |",
            human(bytes),
            bytes as f64 / held as f64,
            ring as f64 / bytes as f64,
            ms[ms.len() / 2],
            match (rss_before, rss_after) {
                (Some(b), Some(a)) => format!("{} -> {}", human(b), human(a)),
                _ => "unavailable".to_string(),
            }
        );
    }
    println!();
    println!(
        "`history_bytes` is the history's buffer capacity. The ring figure is what \
         `(rows + history) * cols * size_of::<Cell>()` comes to; the changelog's \
         \"~35 MB\" is that arithmetic for 10,000 lines at 177 columns."
    );
}

#[derive(Clone, Copy)]
enum Kind {
    Plain,
    Styled,
    Full,
}

/// Scroll `lines` lines through a screen with a `lines`-line history, each
/// written on the bottom row, so that exactly `lines` lines are in history.
fn fill(lines: usize, kind: Kind) -> Screen {
    let mut screen = Screen::with_history(ROWS, COLS, lines);
    let green = Style {
        fg: Color::Indexed(2),
        ..Style::default()
    };
    let mut line = Vec::with_capacity(COLS);
    for i in 0..lines + ROWS {
        line.clear();
        let width = match kind {
            Kind::Full => COLS,
            _ => LINE_LEN,
        };
        // Deterministic, varied text: an agent-style line padded to width.
        let head = format!(
            "[agent-{}] test_{:06} ... ok ({} ms) ",
            i % 8,
            i,
            (i * 7) % 250
        );
        line.extend_from_slice(head.as_bytes());
        while line.len() < width {
            line.push(b'a' + (line.len() % 26) as u8);
        }
        line.truncate(width);
        match kind {
            Kind::Styled => {
                let split = width / 2;
                screen.write_ascii(ROWS - 1, 0, &line[..split], Style::default());
                screen.write_ascii(ROWS - 1, split, &line[split..], green);
            }
            _ => {
                screen.write_ascii(ROWS - 1, 0, &line, Style::default());
            }
        }
        screen.scroll_rows_up(0, ROWS - 1, 1, Cell::default());
    }
    screen
}

fn env_num(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

fn human(bytes: usize) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= MIB {
        format!("{:.2} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Peak resident set of this process, in bytes.
#[cfg(windows)]
fn peak_rss() -> Option<usize> {
    use core::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }
    let mut c = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        ..Default::default()
    };
    // SAFETY: the pseudo-handle for this process needs no closing; the
    // out-struct is sized by its own `cb`, as the API requires.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } != 0;
    ok.then(|| c.peak_working_set_size)
}

/// Peak resident set of this process, in bytes, from `/proc`.
#[cfg(target_os = "linux")]
fn peak_rss() -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: usize = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn peak_rss() -> Option<usize> {
    None
}
