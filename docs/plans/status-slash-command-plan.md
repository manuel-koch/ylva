# Add `/status` slash command (context-origin popup)

## Context

Ylva is a Rust TUI agent harness (crate `ylva`, binary + library). This change adds a `/status`
slash command that opens a modal popup showing where the model's instructions and current context
come from:

- Provider/engine: provider connection, engine name, model, model parameters when available
  (reasoning effort, context length, `max_output_tokens`, …).
- Agent type: its name and configuration (allowed tools).
- Context usage grouped by origin, each with its estimated token contribution as a percentage:
  - system prompt: soul (resolved `SOUL.md` layer: workspace `.ylva/SOUL.md`, Ylva home `SOUL.md`,
    or built-in), trusted instruction files (each with its sentinel source, e.g.
    `$YLVA_HOME/AGENTS.md`, `sub/AGENTS.md`), tool schema description
  - tool calls (call and result)
  - assistant messages
  - user messages
- A coloured horizontal bar graph visualising the percentages.
- `/status` works in the root conversation and in subagent drill-in views (it reports the viewed
  agent).

Motivation: make visible where the model's instructions came from and what consumes the context
window. Token counts are advisory estimates (`cl100k_base`), never billed usage.

Constraints:

- Project rules (`AGENTS.md`): full descriptive names, Apache-2.0 license headers, Rust 2018
  file-per-module (`foo.rs`, no `mod.rs`), thin `main.rs`, items needed by `tests/` re-exported via
  `src/lib.rs`, docs updated in the same commit, no new doc file without asking the user.
- Every slash command's behavior must be reachable consistently; the "every keybound feature also
  exists as a slash command" rule means `/status` needs no keybinding unless added deliberately.
- Existing ADRs (`docs/architecture-design-records/`, 0001–0003) were checked: none covers popups or
  slash commands; ADR 0003 (workspace trust by content hash) is relevant only in that the status
  view must not expose untrusted/unloaded instruction content.

Definition of done:

- `/status` appears in the overlay, parses, and opens the popup from root and subagent views,
  including while busy.
- Popup shows all sections above with correct values for the viewed agent; percentages are
  model-view tokens per category as a share of the effective context window, with a grey "free"
  segment; bar graph rendered with distinct colours and a legend.
- Popup is dismissible (`esc`), scrollable when taller than the terminal, and consistent with the
  Agents popup interaction model.
- Unit tests, reducer/presenter tests and render tests added; `make format lint` and
  `make test-coverage` pass; docs updated.

## Implementation Plan

Implementing agent: flip `[ ]` to `[x]` immediately after completing each step and rewrite this file
in place so progress is resumable across sessions.

1. [x] Read `docs/development-workflow.md` and `docs/known-test-issues.md` (tracing, cwd and
       integration-test quirks) before writing tests.
2. [x] Define a pure `ContextBreakdown` model (new file `src/context_breakdown.rs`, re-exported in
       `src/lib.rs`): origin categories (soul, instruction file with source, tool schemas, tool
       calls and results, assistant messages, user messages, compaction summary/recovery handoff;
       tool calls and results form one category), token counts, and a function computing
       percentages. Write its unit tests first (TDD): percentage rounding, zero total, ordering.
3. [x] Make the system prompt's origins recoverable. Extend `src/config/instructions.rs`
       (`compose_root_system_prompt`, `compose_soul_with_instructions`, `InstructionFile::source`)
       and `src/config/soul.rs` (`load_soul`) so the composer also returns structured parts (soul
       text + resolved soul layer, ordered instruction files with sources, safety preamble) while
       producing the identical prompt string. Keep one source of truth: the string is derived from
       the parts.
4. [x] Carry those parts (and persona/skill advertisement from `compose_agent_system_prompt` in
       `src/agent_factory.rs`) to where the TUI can read them: attach a `SystemPromptOrigins` value
       to the agent construction path, per agent (root and each subagent).
5. [x] Compute the breakdown from an agent's `Context` (`src/agent/context.rs`: `messages()`,
       `token_count(index)`, `leading_system_len()`, `tool_schema_token_count`,
       compaction/model-view accounting) using the system prompt origins; estimate each origin with
       `TokenEstimator::estimate_tokens` (`src/llm/token_estimate.rs`).
6. [x] Add a driver effect/event pair (e.g. `Effect::ShowStatus` → `AppEvent` carrying a
       `StatusSnapshot` with engine info, agent type config and `ContextBreakdown`) handled in
       `src/tui_driver.rs`, since engine/agent state lives in the driver, not the reducer. Support
       subagent views by targeting the viewed agent.
7. [x] Register the command in `src/slash_command.rs`: add `STATUS_COMMAND_NAME`,
       `SlashCommand::Status`, a `SLASH_COMMANDS` entry (no argument) and extend the
       parsing/suggestion tests.
8. [x] Handle `SlashCommand::Status` in `src/presenter/keyboard.rs` (next to `SlashCommand::Agents`,
       incl. the busy-allowed match around line 798): clear the textarea and emit the status effect.
9. [x] Add popup state and focus: `Focus::StatusPopup` (modal, `src/tui_reducer_state/events.rs`),
       `StatusPopupState` in a new `src/tui_reducer_state/status_popup.rs` (snapshot, scroll
       offset), wiring into `AppState` (`app_state.rs`), keyboard/mouse routing (`esc` close,
       arrows/page/wheel scroll) and layout (`src/tui_reducer_state/layout.rs`).
10. [x] Render the popup in a new `src/tui_widgets/status_popup.rs` (model on `agents_popup.rs`):
        sections for provider/engine, agent type, usage table, and a coloured horizontal stacked bar
        with legend; add theme colours in `src/theme.rs`.
11. [x] Add tests: reducer/presenter tests for opening, closing, scrolling, busy and subagent cases;
        render tests for the popup and bar graph; slash parser/suggestion tests; a context-breakdown
        test against a hand-built `Context`.
12. [x] Update docs in the same change: `docs/features.md` (slash-command table and a `/status`
        description), `docs/implementation-notes.md` (Slash commands section: effect/event flow,
        breakdown model, prompt origins), `docs/keybindings.md` (popup keys), `CONTEXT.md` (terms
        such as "Context breakdown"), and, after asking the user, delete
        `docs/ideas/slash-commands.md` (it is empty once `/status` is removed) and drop references
        to it. Run `rumdl` on changed markdown.
13. [x] Verify: `make format lint`, `make test-coverage` (maintain the coverage threshold from
        `docs/development-workflow.md`); `make audit` only if dependencies changed.

## Implementation Hints

- Slash commands live in `src/slash_command.rs`: constants (`AGENTS_COMMAND_NAME`…),
  `enum SlashCommand`, `SLASH_COMMANDS` allowlist (fields `name`, `aliases`, `description`,
  `command`, `accepts_argument`), `parse_slash_command_invocation`, `slash_command_suggestions`.
- `/agents` is the closest analogue: `src/presenter/keyboard.rs` (~line 580) calls
  `state.open_agents_popup()` and returns `Effect::ClearTextarea`; it is allowed while busy (~line
  798). Agents popup state: `src/tui_reducer_state/agents_popup.rs`; widget:
  `src/tui_widgets/agents_popup.rs`; `Focus::AgentsPopup` is modal (`Focus::is_modal` in
  `src/tui_reducer_state/events.rs`); layout struct `AgentsPopupLayout` in `layout.rs`.
- Engine-dependent slash effects are handled by the driver, not the reducer
  (`docs/implementation-notes.md`, "Slash commands").
- System prompt composition: `compose_root_system_prompt` / `compose_soul_with_instructions` in
  `src/config/instructions.rs` (SOUL, then `INSTRUCTION_SAFETY_PREAMBLE`, then one tagged block per
  `InstructionFile` with sentinel source; `$YLVA_HOME/<file>` for Ylva home files,
  workspace-relative path otherwise). `compose_agent_system_prompt` in `src/agent_factory.rs`
  appends agent persona and the skill advertisement. SOUL is loaded by `load_soul(workspace_root)`
  in `src/config/soul.rs`; layer precedence is workspace, Ylva home, built-in (`DEFAULT_SOUL`).
- Token accounting: `Context` (`src/agent/context.rs`) caches per-message counts (`token_count`,
  `approximate_token_count`), `model_view_token_count`, `tool_schema_token_count`,
  `estimated_model_tokens(tools, engine)`; the leading system block is `leading_system_len()`
  messages. Footer usage formatting: `src/tui_reducer_state/context_usage.rs`
  (`format_context_usage`), `format_token_estimate` in `src/llm/token_estimate.rs`. The estimator is
  advisory `cl100k_base` (see `CONTEXT.md` "estimates … never billed usage").
- Message variants are in `src/agent/message.rs` (`enum Message`, with `System`); tool schemas via
  `src/agent/tools.rs` (`schemas()`, `ToolSchema`).
- Domain terms to reuse: Context, System prompt, Model view, Transcript, Trusted instruction file,
  Global SOUL, Agent type (`CONTEXT.md`).
- Subagents have their own `Context`, agent type and engine; subagent drill-in uses
  `ConversationView` (see `docs/implementation-notes.md`).
- Resolved decisions: percentages are relative to the effective context window
  (`Engine::effective_context_length`), matching the footer, with a "free" segment; a compaction
  summary/recovery handoff is its own category; model parameters that are not configured or reported
  are omitted (context length is always shown); `/status` reports only the viewed agent (root or
  drilled-in subagent), with no subagent aggregate; no keybinding is added; tool calls and results
  are one category; `docs/ideas/slash-commands.md` is deleted after user confirmation.
- The popup shows the soul layer and instruction files by sentinel source only
  (`$YLVA_HOME/SOUL.md`, `.ylva/SOUL.md`, built-in, `sub/AGENTS.md`), never absolute paths.
