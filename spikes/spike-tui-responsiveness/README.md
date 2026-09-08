# Spike 1: TUI Responsiveness Under Load

A standalone proof-of-concept (spike) crate that validates the core
responsiveness claim behind the Ylva design: a `ratatui` + `tokio` TUI can stay
at ~60 FPS while an artificial LLM stream injects tokens at very high volume and
while the user steers the stream mid-flight.

This is a **spike, not production code**. It lives in the dedicated `spikes/`
directory, is a separate crate, and is not wired into any future `src/`
tree (see `docs/blueprint.md` §Phase 0 / §Spike 1). The full intent and the
acceptance criteria are recorded in [plan.md](./plan.md).

## What it proves

- The TUI thread never blocks on I/O: crossterm input is read on a
  `spawn_blocking` thread and bridged to the async loop via a channel +
  `tokio::select!` pattern.
- Tokens arrive over a **bounded** `mpsc` channel from a generator task; the
  draw loop **batch-drains** all available tokens per frame via `try_recv()`.
- Redraw is event-driven (`terminal.draw()` runs only when a channel fires),
  and frame time / queue depth / peak buffer memory are recorded on every draw.
- Rapid steering: a mid-stream `Prompt` cancels the current generator
  (`CancellationToken`), awaits its completion, and starts a fresh one.

## Build

Requires a Rust/Cargo toolchain (cargo ≥ 1.94). Build inside this directory:

```shell
cargo build                  # debug
cargo build --release        # optimized (recommended for timing)
```

The binary name is `ylva-spike-tui-responsiveness`:

```shell
cargo run --release
```

## Run

The spike is a **TUI** and must be run from a real interactive terminal
(`ratatui::init()` fails without a TTY, e.g. under a plain pipe).

```shell
cargo run --release                          # default: 60s soak @ 1000 msg/s
cargo run --release -- --duration 10         # 10-second run
cargo run --release -- --rate 2000           # 2000 msg/s
cargo run --release -- --duration 600        # mandated 10-minute soak
```

### Options

| Flag | Default | Meaning |
| --- | --- | --- |
| `--duration <secs>` | `60` | Run length. Use `600` for the 10-minute soak. |
| `--rate <n>` | `1000` | Artificial token injection rate (msg/s). |

### Controls

- `Enter` — steer: cancel the current stream and start a fresh one.
- `Ctrl+C` — stop: cancel the current stream (stays idle).
- `q` — quit early (the run otherwise auto-exits at `--duration`).

A `Prompt` is also auto-fired ~3 s after start so mid-stream steering is
demonstrated on a plain run without interaction.

### Metrics

On exit the spike prints a summary to stderr with the success criteria:

- frame-time stats: total frames, count > 17 ms, min/avg/max, and a histogram
  (`<8`, `8–16`, `16–33`, `>33` ms);
- **queue depth**: peak channel depth (target ≤ ~20 tokens at 1000 msg/s —
  no consumer starvation);
- **peak buffered memory**: the ring buffer's highest observed byte length
  (flat over a soak run — no growth).

The live status line at the top of the TUI shows the same numbers during the
run.

## Test / lint

```shell
cargo test      # unit tests for the pure logic (app.rs ring + frame-window math)
cargo clippy    # lint
cargo fmt       # format
```

The core FPS/input criteria are manual performance checks that require a live
terminal; only the terminal-free logic in `src/app.rs` is unit-tested.
