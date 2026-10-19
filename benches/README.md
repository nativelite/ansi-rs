# nativelite-ansi benchmarks

```sh
cargo bench --bench history_memory
cargo bench --bench feed_throughput
```

Both are release builds on the standard library alone (`harness = false`, a
`main()` timing with `std::time::Instant`). Under `cargo test --all-targets`
each target only builds and returns, so the local gate stays fast. Every
number in `CHANGELOG.md` that one of these benches reproduces names the bench
beside it; numbers measured elsewhere name their source.

## `history_memory`: what scrollback costs

A 177x47 `Screen::with_history(.., 10_000)` has 10,000 lines scrolled off
its top, each written with `Screen::write_ascii` on the bottom row and
scrolled with `scroll_rows_up` over the whole screen (the only scroll that
feeds the history). It reports:

- **`history_bytes`**: `Screen::history_bytes()`, the capacity of the
  history's buffers. The number the crate answers for.
- **bytes/line**, and **vs ring of cells**: how many times smaller than
  `(rows + history) * cols * size_of::<Cell>()`, which is what the pre-0.4.1
  layout (a ring of full rows) allocated for the same shape. That arithmetic
  is where the changelog's "~35 MB" comes from.
- **fill ms**: time to write and scroll the lines, median of the runs.
- **peak RSS before -> after** (Windows and Linux): the process's peak
  resident set, a rough cross-check that the heap agrees. It includes
  allocator slack; read it as an order of magnitude.

Three line shapes: 48-character one-style lines (the usual agent line, stored
by copying its bytes), 48-character lines with a colour change mid-line
(stored as text plus style runs), and full-width 177-character lines.

Knobs: `ANSI_BENCH_LINES` (default 10000), `ANSI_BENCH_RUNS` (default 5).

## `feed_throughput`: MiB/s through the parser and the screen

A deterministic, agent-style stream (coloured test-runner lines, CRLF, a
cursor move and erase every 50 lines, an OSC title every 500; 64 MiB by
default) is fed in 8 KiB chunks, as a pty reader delivers output. Rows:

- **`Parser::feed_with`**: the tokenizer alone, events counted, nothing
  allocated.
- **`Parser::feed`**: the same machine building owned `Token`s (a `String` or
  `Vec` per token, UTF-8 validation of text).
- **`screen ...`**: `feed_with` driving a 177x47 `Screen` through the
  smallest interpreter that exercises the hot paths (text via `write_ascii`
  with autowrap, LF scrolls at the bottom, CR, SGR, CUP; everything else
  dropped). This is **not** a terminal emulator; `nativelite-vterm` is. These
  rows measure this crate's share of emulation. Three shapes: no history with
  a full-screen scroll, a 10,000-line history, and a 10,000-line history with
  a one-row status line (a `DECSTBM` scroll region, which never feeds the
  history).

Each row is the median of the runs, with the min and max beside it.

Knobs: `ANSI_BENCH_MB` (default 64), `ANSI_BENCH_RUNS` (default 5),
`ANSI_BENCH_ONLY` (`parse` or `screen`).

## Reading the numbers

State the machine when you quote one. The changelog's `(cargo bench --bench
<name>: ...)` notes were measured on the founder's desktop (16 logical cores,
Windows 11) in a release build; treat single-digit percentage differences as
noise until repeated. What these benches do **not** measure: a real terminal
emulator's full cost (`nativelite-vterm`), ptys, rendering, or anything about
`Screen::diff`.
