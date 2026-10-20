# Ylva

Ylva (Old Norse for "she-wolf") is a Rust TUI and CLI agent-harness
that coordinates model tool calls and schedules subagent runs.

## Getting started

Install a current stable [Rust toolchain](https://rustup.rs/) (including Cargo)
and native build tools for your platform. From a checkout of this repository:

```shell
cargo install --path . --locked
```

Ensure Cargo's installation directory (normally `~/.cargo/bin`) is on `PATH`.
Inspect the available commands, then launch Ylva in a terminal to open the TUI:

```shell
ylva --help
ylva
```

For real model responses, configure Ollama, OpenRouter or GitHub Copilot using
the
[provider environment variables](docs/cli-options.md#environment-variables)
or [profile configuration](docs/configuration.md#profile-file).
To add a provider interactively instead, run `ylva connect <provider> [--name <name>]`
(use provider `ollama`, `openrouter` or `copilot`); see
[connect commands](docs/cli-options.md#connect-commands).
Without configured engines or providers, Ylva uses a deterministic mock provider,
not a real model.

## Documentation

- [CLI reference](docs/cli-options.md): flags, subcommands, and headless usage.
- [Configuration](docs/configuration.md): profiles, engines, provider connections,
  and approval rules.
- [Keybindings](docs/keybindings.md): TUI controls.
- [Provider notes](docs/provider-notes.md): what each provider reports, such as
  usage and costs.
- [Features](docs/features.md): implemented behavior.

## Contribution

- [Domain language](CONTEXT.md): canonical project terminology.
- [Development workflow](docs/development-workflow.md): build, install, and test
  targets; [implementation notes](docs/implementation-notes.md) explain internals.

## Ylva's features

Ylva aims to support lean set of features.

Ylva is highly customizable to adapt to user needs.

- [x] Global and workspace scoped configurations
- [x] Customize behavior using `profiles` on global and and workspace scope
  - [x] Profile shims (`ylva-<name>`) to run a profile by name; add the shims
        directory to `PATH`
- [x] Layered global/workspace configuration with `ylva init` scaffolding
- [x] A `SOUL.md` persona, and trusted `AGENTS.md` / `CLAUDE.md` instruction
      files folded into the system prompt
- [x] Local Agent Skills with per-agent catalogs
- [x] Multiple LLM providers built-in
  - [x] Ollama
  - [x] Openrouter
  - [x] Copilot
  - [ ] Anthropic
- [x] Configurable `engines` ( combination of provider + model + config )
- [x] Tracking billed token usage and cost totals
- [x] Session / context compaction
- [x] Concurrent subagent execution with a profile-configurable limit
- [x] Support LLM calling tools:
  - [x] `read`, `edit` & `write` file access
  - [x] `search` for file content using regexp
  - [x] `glob` for path search using glob-pattern
  - [x] `bash` to run arbitrary commands in a shell
  - [x] `clarify` to ask user interactive question
  - [x] `spawn_subagent` to deligate workload to a spawned subagents
  - [x] `web_search` to search for terms in www, using backends
    - [x] DuckDuckGo
    - [x] SearXNG
    - [x] Brave
  - [x] `web_fetch` to fetch data from a given URL
  - [x] `load_skill` to load named Agent Skill instructions on demand
- [x] Human-in-the-loop safety via a path-based tool-approval sandbox
- [x] Per-tool configurable sandbox approval rules
- [x] Persistent session storage
  - [x] Ability to resume an existing session
  - [x] Cloning an existing session into a new one (`--clone`)
  - [x] User message history, to reuse a message/prompt from a recent session
  - [x] Automatic title generation for a session
  - [x] Root agent type selection (`--agent`), restored on resume
  - [x] Per-agent tool-use iteration limit (`max_tool_iterations`)
  - [x] Session listing and explicit deletion with dry-run previews
  - [x] Manual lossless archival/restoration, including archived resume and clone
- [x] TUI drill-in view to inspect a spawned subagent's own conversation
- [x] Slash commands in TUI
  - [x] `/models` to switch named engines or provider-catalog models
  - [x] `/clear` to clear/reset current session
  - [x] `/session` to resume or clone a recent root session without exiting
  - [x] `/compact` to manually summarize the root model view, retaining the transcript
