# Ylva

Ylva (Old Norse for "she-wolf") is a Rust TUI and CLI agent-harness
that coordinates model tool calls and schedules subagent runs.

## Ylva's features

Ylva aims to support lean set of features.

Ylva is highly customizable to adapt to user needs.

- [x] Global and workspace scoped configurations
- [x] Customize behavior using `profiles` on global and and workspace scope
- [x] Layered global/workspace configuration with `ylva init` scaffolding
- [x] A `SOUL.md` persona, and trusted `AGENTS.md` / `CLAUDE.md` instruction
      files folded into the system prompt
- [x] Multiple LLM providers built-in
  - [x] Ollama
  - [x] Openrouter
  - [ ] Copilot
  - [ ] Anthropic
- [x] Configurable `engines` ( combination of provider + model + config )
- [x] Ability to run main and subagent(s)
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
- [x] Human-in-the-loop safety via a path-based tool-approval sandbox
- [x] Per-tool configurable sandbox approval rules
- [x] Persistent session storage
  - [x] Ability to resume an existing session
  - [x] Cloning an existing session into a new one (`--clone`)
  - [x] User message history, to reuse a message/prompt from a recent session
  - [x] Automatic title generation for a session
  - [x] Root agent type selection (`--agent`), restored on resume
  - [x] Per-agent tool-use iteration limit (`max_tool_iterations`)
- [x] TUI drill-in view to inspect a spawned child agent's own conversation
- [x] Slash commands in TUI
  - [x] `/models` to switch named engines or provider-catalog models
  - [x] `/clear` to clear/reset current session
- [ ] Tracking billed token usage and cost totals in current context (an
      approximate context-size readout is already shown in the TUI footer)
- [x] Session / context compaction
- [ ] Support for skills
