# Spike 2: HTTP Stream Cancellation

A standalone proof-of-concept (spike) crate that validates the cancellation
claim behind the Ylva design: a raw `reqwest` + `eventsource-stream` SSE stream
can be aborted **cooperatively** such that the underlying TCP connection closes
immediately (no `CLOSE_WAIT` / `ESTABLISHED` linger) and the tokio runtime leaks
**zero** tasks. This is what lets a user hit `Ctrl+C` or steer mid-stream
without leaking sockets or background tasks in the real agent.

This is a **spike, not production code**. It lives in the dedicated `spikes/`
directory, is a separate crate, and is **not** wired into any future `src/`
tree (see `docs/blueprint.md` §Phase 0 / §Spike 2). The full intent and the
acceptance criteria are recorded in [plan.md](./plan.md).

Unlike Spike 1 this experiment is **headless** (a plain binary, no TUI):
cancellation and connection state are verified programmatically and via OS
socket inspection, so no live terminal is required.

## What it proves

- Dropping the stream future aborts the underlying HTTP connection
  immediately (no `CLOSE_WAIT` / `ESTABLISHED` linger).
- The tokio runtime shows zero leaked tasks after a full sweep.
- `CancellationToken` integration works reliably with a `biased` select
  (no race between `cancelled()` and `stream.next()`).

Three cancellation mechanisms are compared to adjudicate the Pitfall #8
question from `docs/blueprint.md` §8 / §Spike 2:

1. `drop:adapter-only` — drop only the `eventsource`/`bytes_stream` adapter.
2. `drop:adapter+response` — drop the adapter and the `Response` together.
3. `token` — cooperatively abort via a `CancellationToken` polled in a `biased`
   `tokio::select!`, mirroring `docs/blueprint.md` §4A.

## Build

Requires a Rust/Cargo toolchain (cargo ≥ 1.94). Build inside this directory:

```shell
cargo build                  # debug
cargo build --release        # optimized (recommended)
```

The binary name is `ylva-spike-http-stream-cancel`.

## Run

The default run targets the **hermetic local mock SSE server** (no network, no
API keys):

```shell
cargo run --release
```

The mock server streams OpenAI-style `data: {...}\n\n` frames on a 10 ms tick,
accepts one connection at a time, and reports a live-connection counter so the
client side can confirm the server released the connection too.

### Options

| Flag | Default | Meaning |
| --- | --- | --- |
| `--endpoint <mock\|openrouter\|ollama>` | `mock` | Target endpoint. |
| `--base-url <url>` | per endpoint | Base URL for real endpoints. |
| `--model <name>` | per endpoint | Model name for real endpoints. |
| `--cancel-mode <drop\|token>` | `token` | Cancellation mechanism. |
| `--drop-subcase <adapter-only\|adapter-response>` | — | Direct-drop sub-case (implies `drop`). |
| `--cancel-at <fraction\|events>` | `0.5` | `0 < x ≤ 1` fraction of the frame budget, else an absolute event offset. |
| `--iterations <n>` | `5` | Runs per single-offset / sweep fraction. |
| `--sweep-fractions <a,b,…>` | `0.1,0.3,0.5,0.7,0.9` | Fractions swept. |
| `--frame-cap <n>` | `300` | Mock-server frame budget (`0` streams forever). |
| `--race-sweep` / `--no-race-sweep` | on | Aggressive token race sweep (token mode only). |
| `--race-iterations <n>` | `300` | Iterations for the race sweep. |
| `--race-offset-events <n>` | `6` | Small event offset used by the race sweep. |
| `--print-events <n>` | `0` | Dump up to `n` raw SSE `data:` payloads per single run (0 disables). |

**Keys are never hard-coded.** For `--endpoint openrouter` read `OPENROUTER_API_KEY`
from the environment; local Ollama uses `--base-url` (default `http://localhost:11434`).
The default/CI run targets only the hermetic mock server.

Examples:

```shell
# Default: mock server, token cancel, sweep + 300-iteration race sweep.
cargo run --release

# Direct-drop comparison focusing on adapter-only (Pitfall #8).
cargo run --release -- --cancel-mode drop --drop-subcase adapter-only

# Ollama (OpenAI-compatible /v1/chat/completions).
cargo run --release -- --endpoint ollama

# OpenRouter (requires OPENROUTER_API_KEY).
cargo run --release -- --endpoint openrouter
```

To see the actual content fetched from the server (rather than the stats
only), dump the raw SSE `data:` payloads that the client receives on the
single-offset runs:

```shell
cargo run --release -- --cancel-at 4 --frame-cap 20 --print-events 6 --no-race-sweep
```

## Success criteria

On exit the spike prints a per-criterion `PASS`/`FAIL` summary to stderr:

- **no lingering connection** — server-side live-connection counter returns to
  `0` and `lsof`/`netstat` show no `ESTABLISHED`/`CLOSE_WAIT` entry for the port;
- **zero leaked tasks** — runtime `num_alive_tasks()` returns to the pre-run
  baseline, with a clean `shutdown_timeout` as a secondary signal;
- **no `CancellationToken` race** — after `cancelled()` is observed the `select`
  breaks and at most one in-flight chunk is delivered before the stream stops.

A direct-drop vs token-abort comparison table reports per-mechanism mean/p90
close-latency and whether each sub-case closed the connection (this adjudicates
`docs/blueprint.md` §8 Pitfall #8). The process exits non-zero if any criterion
fails, so the spike is CI-assertable.

## Test / lint

```shell
cargo test      # unit + integration-style tests (boots the mock server)
cargo clippy    # lint
cargo fmt       # format
```
