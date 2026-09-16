# Ylva

Ylva (Old Norse for "she-wolf") is a Rust TUI and CLI agent-harness
that coordinates model tool calls and schedules subagent runs.

## Ylva's features

Ylva aims to support lean set of features.

Yvla is not loaded with bloat of barely used functionality you may find in other agent harness implementations.

Ylva is highly customizable to adapt to user needs.

- [x] Global and workspace scoped configurations
- [x] Customize behavior using `profiles` on global and and workspace scope
- [x] Layered global/workspace configuration with `ylva init` scaffolding and a `SOUL.md` persona
- [x] Multiple LLM providers built-in
  - [x] Ollama
  - [x] Openrouter
  - [ ] Copilot
- [x] Configurable `engines` ( combination of provider + model + config )
- [x] Ability to run main and subagent(s)
- [x] Support LLM calling tools:
  - [x] `read`, `edit` & `write` file access
  - [x] `search` for file content using regexp
  - [x] `glob` for path search using glob-pattern
  - [x] `bash` to run arbitrary commands in a shell
  - [x] `clarify` to ask user interactive question
  - [x] `spawn_subagent` to deligate workload to a spawned subagents
  - [ ] `web_search` to search for terms in www
  - [ ] `web_fetch` to fetch data from a given URL
- [x] Human-in-the-loop safety via a path-based tool-approval sandbox
- [ ] Per-tool configurable sandbox approval rules
- [x] Persistent session storage
  - [x] Ability to resume an existing session
  - [x] User message history, to reuse a message/prompt from a recent session
  - [x] Automatic title generation for a session
- [x] TUI drill-in view to inspect a spawned child agent's own conversation
- [x] Slash commands in TUI
  - [x] `/models` to change current provider + model
  - [x] `/clear` to clear/reset current session
- [ ] Tracking token usage in current context
- [ ] Session / context compaction
