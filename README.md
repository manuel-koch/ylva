# Ylva

Ylva (Old Norse for "she-wolf") is a Rust TUI and CLI agent-harness
that coordinates model tool calls and schedules subagent runs.

## Status and support

> [!NOTE]
> Ylva is a personal, experimental project maintained on a best-effort basis,
> with no response-time guarantee. Do not depend on it for critical work.

- **Platform:** macOS only. Linux is unverified; Windows is unsupported.
- **Installation:** from source only (see below).
- **Providers:** integrations are best-effort. No provider is primary and no
  model quality or compatibility is guaranteed; see
  [provider notes](docs/provider-notes.md#integration-and-model-quality).
- **Modalities:** model output is text only. Generating images, audio, or
  other non-text output is out of scope for now. Reading images and PDFs as
  tool results works only with models that support it.
- **Bugs and questions:** open a
  [GitHub issue](https://github.com/manuel-koch/ylva/issues).
- **Vulnerabilities:** do not post them in public issues. Use GitHub's private
  vulnerability reporting for this repository
  ([Security tab](https://github.com/manuel-koch/ylva/security)).

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

A model provider must be configured before a conversation can start; without
one, Ylva exits with setup guidance (exit code 2). Configure Ollama, OpenRouter
or GitHub Copilot using
the
[provider environment variables](docs/cli-options.md#environment-variables)
or [profile configuration](docs/configuration.md#profile-file).
To add a provider interactively instead, run `ylva connect <provider> [--name <name>]`
(use provider `ollama`, `openrouter` or `copilot`); see
[connect commands](docs/cli-options.md#connect-commands).
Help, version, and the `profile`, `connect`, `init` and `session` commands work
without a provider.

Provider setup: [Ollama](https://ollama.com/) (local or cloud) via
`ylva connect ollama`, or [OpenRouter](https://openrouter.ai/) via
`ylva connect openrouter`.

> [!WARNING]
> The GitHub Copilot integration is unofficial. It relies on an undocumented
> API and impersonates the VS Code Copilot Chat client (client id and editor
> headers). It may break without notice and may conflict with GitHub's terms;
> GitHub can suspend Copilot access for abuse or excessive requests. Use it at
> your own risk; see [provider notes](docs/provider-notes.md#github-copilot).

## Safety and data

- **Approvals are policy checks, not isolation.** The tool-approval "sandbox"
  decides which tool calls need your consent. It does not confine processes or
  the filesystem at the OS level, and `bash` approval rules are heuristics. An
  approved command runs with your user's full permissions and environment; see
  [features](docs/features.md#tools-to-be-used-by-models) and
  [configuration](docs/configuration.md#sandbox-tool-approval-rules).
- **Auto-approval removes the human check.** `--auto-approve-*` flags and
  catch-all approval rules let the model act without asking. Use them only in
  disposable or otherwise trusted environments.
- **Provider data sharing.** With a hosted provider (OpenRouter, Ollama Cloud,
  Copilot), prompts, file contents read by tools, tool output, and web results
  are sent to that provider. Only a local Ollama server keeps them on your
  machine. OpenRouter requests also carry attribution headers that identify
  Ylva as the calling application.
- **Local transcripts.** Sessions are stored unencrypted under `~/.ylva/chats`
  (workspace-scoped profiles may use `<workspace>/.ylva/chats`) and may contain
  secrets that appeared in a conversation; see
  [persistence](docs/persistence.md).
- **Diagnostic dumps are sensitive.** `YLVA_REQUEST_DUMP_PATH` writes request
  and response bodies, including conversation content, to disk; see
  [CLI options](docs/cli-options.md#http-request-and-response-dumps-ylva_request_dump_path).

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
  targets, [git hooks and commit/branch conventions](docs/development-workflow.md#git-hooks-and-conventions)
  (run `make install-git-hooks`); [implementation notes](docs/implementation-notes.md) explain internals.

## Ylva's features

Ylva aims to support lean set of features.

Ylva is highly customizable to adapt to user needs.

- [x] Global and workspace scoped configurations
- [x] Customize behavior using `profiles` on global and and workspace scope
  - [x] Profile shims (`ylva-<name>`) to run a profile by name; add the shims
        directory to `PATH`
- [x] Layered global/workspace configuration with `ylva init` scaffolding
- [x] Workspace configuration blocks startup until approved with `ylva trust`,
      so a cloned repository cannot redirect credentials or disable approvals
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
