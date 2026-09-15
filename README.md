# Ylva

Ylva (Old Norse for "she-wolf") is a Rust TUI and CLI agent-harness
that coordinates model tool calls and schedules subagent runs.

## Ylva's features

Ylva aims to support lean set of features.

Yvla is not loaded with bloat of barely used functionality you may find in other agent harness implementations.

Ylva is highly customizable to adapt to user needs.

- [x] global and workspace scoped configurations
- [x] customize behavoir using `profiles` on global and and workspace scope
- [x] Ability to run main and subagents
- [x] Support LLM calling tools:
  - [x] `read`, `edit` & `write` file access
  - [x] `search` for file content using regexp
  - [x] `glob` for path search using glob-pattern
  - [x] `bash` to run arbitrary commands in a shell
  - [x] `clarify` to ask user interactive question
  - [x] `spawn_subagent` to deligate workload to a spawned subagents
- [x] Multiple LLM providers built-in
  - [x] Ollama
  - [x] Openrouter
