# Single agent persona (body = persona)

An implementing agent must flip `[ ]` to `[x]` immediately after completing
each step and rewrite this sidecar file in place, so progress is resumable
across sessions.

## Context

An agent type's model-facing text is currently split over two fields of
`AgentConfig`:

- `persona` — a short instruction available from a `.toml` agent-type file,
  from `.md` frontmatter, and from an inline `[agents.<type>]` table;
- `system_prompt` — the "file body": the Markdown body of a `.md` agent-type
  file, and the `system_prompt` key of a `.toml` agent-type file. Inline
  tables cannot set it.

`compose_agent_system_prompt` appends both, in that order, after the global
baseline. To the model the two are indistinguishable — both are plain text
sections — so the split buys nothing at prompt level and costs two knobs for
one concept: a `.md` file can express the same property twice (frontmatter
`persona` + body, with the body silently winning for `system_prompt`), and
inline config can only ever reach one of the two fields.

Settled design (reviewed):

- **One prompt property per agent type: `persona`.** It is composed as a
  single section after the global baseline (global SOUL + trusted instruction
  sections) and before the tool protocol. It extends the baseline; it never
  replaces it.
- Surfaces, all expressing the same property:
  - `.toml` agent-type file: `persona = "..."` (plain `AgentConfig`
    deserialization);
  - `.md` agent-type file: the **body** — the frontmatter is deserialized
    with the standard `AgentConfig` struct, then the trimmed body overrides
    `persona`;
  - inline `[agents.<type>]` in profile/workspace `config.toml`:
    `persona = "..."`, a field-level override that *replaces* a file-sourced
    persona (other fields keep merging individually).
- A `.md` file whose body is empty or whitespace-only is a hard startup error:
  a body-less Markdown agent file is pointless, and with frontmatter `persona`
  no longer meaningful there is nothing left for it to say. Use a `.toml`
  file for settings-only definitions.
- Frontmatter `persona` in a `.md` file is silently overridden by the body —
  no dedicated detection (consistent with the migration decision below).
- `system_prompt` is removed with **no** backward-compatible handling and no
  rename detection: existing `system_prompt = "..."` entries become unknown
  keys. The docs state the new status quo only; no migration notes.
- Out of scope: `SOUL.md` layering, trusted instruction files,
  `AgentFactory::with_system_prompt` (the global baseline), the same-scope
  file/inline ambiguity guard, `AgentConfig::description` (the
  `spawn_subagent` picker text), and `AgentFactory`'s own `system_prompt`
  field (it holds the global composed prompt).

Acceptance criteria:

- Exactly one agent-type prompt field exists (`AgentConfig::persona`); a repo
  grep for `system_prompt` in `src/`/`tests/` only finds global-prompt uses
  (`AgentFactory`/`AgentLoop`/`compose_root_system_prompt`/`main.rs`).
- The composed prompt order — global baseline, agent-type `persona`, tool
  protocol (then, for a subagent, its briefing and runtime constraints) — is
  one agent section, documented in `docs/configuration.md`,
  `docs/features.md`, `docs/implementation-notes.md`, and `CONTEXT.md`.
- `.toml` file `persona`, `.md` body, and inline `persona` all populate the
  same field; an inline `persona` replaces the file-sourced persona while
  other inline fields (`timeout_secs`, `can_spawn`, …) still merge
  individually.
- A `.md` agent-type file without a non-empty body fails with
  `ConfigError::MalformedAgentType` naming the body requirement; a frontmatter
  `persona` next to a body has no effect.
- Same-scope file + inline definitions still fail with
  `AmbiguousAgentTypeDefinition`; a profile inline override of a lower-scope
  file remains valid.

Key constraints:

- Composition stays additive (`compose_agent_system_prompt` joins with
  `"\n\n"`); only the number of agent sections changes (two → one).
- Inline tables keep using `AgentConfigOverrides` (it already carries only
  `persona` among prompt fields — no structural change there).
- `docs/blueprint.md` and this plan file are historical records and keep
  their period-correct wording.
- No configuration knobs, no serde aliases, no `deny_unknown_fields`.

## Implementation Plan

1. [x] Model: in `src/config/agent.rs` delete the `system_prompt` field from
   `AgentConfig` and re-document `persona` as *the* agent-type prompt
   property: composed after the global baseline (global SOUL + trusted
   instruction sections) and before the tool protocol, extending rather than
   replacing it; settable from a `.toml` agent-type file, a `.md` body, or an
   inline `[agents.<type>]` override. Remove the now-stale note that inline
   tables "do not set this field".
2. [x] `.md` parsing: in `src/config/agent_type_file.rs`
   (`parse_markdown`) keep deserializing the frontmatter with the standard
   `AgentConfig`, then replace `config.system_prompt = Some(body.trim()…)`
   with: trim the body; on empty/whitespace return
   `ConfigError::MalformedAgentType` ("Markdown agent-type file has no body;
   the body is the agent persona (use a `.toml` file for settings-only
   definitions)"); otherwise `config.persona = Some(trimmed)`. Unit tests:
   body overrides a frontmatter `persona`; a body-less `.md` file fails with
   the documented error; a `.toml` file's `persona = "..."` parses into the
   same field (rename
   `pure_toml_agent_type_files_parse_settings_and_system_prompt` →
   `…_parse_settings_and_persona`); existing body assertions read `.persona`;
   the `research.toml` fixture in the duplicate-extension test uses `persona`.
3. [x] Inline/profile path: in `src/config/profile.rs` drop `system_prompt`
   from `default_agent_config()`, the inline-entry initializer, and test
   literals; rename agent-file fixtures `system_prompt = "…"` → `persona = "…"`
   and retarget their assertions to `.persona`. Rewrite
   `inline_agent_fields_override_a_file_definition_without_replacing_its_body`
   → `inline_agent_fields_override_a_file_definition`: the inline
   `persona = "Be concise."` now replaces the file body persona while
   `timeout_secs`/`can_spawn` keep merging. Rewrite
   `inline_profile_override_of_a_global_agent_type_is_allowed` to assert the
   inline persona is the only persona left. Ambiguity tests unchanged.
4. [x] Composition and remaining literals: in `src/agent_factory.rs` remove
   the second push in `compose_agent_system_prompt` (the
   `agent_config.system_prompt` branch), collapse fixtures that set both
   fields to the single `persona`, and fix expected strings
   (`"global soul\n\nspecialist persona\n\nspecialist body"` → one section).
   Remove `system_prompt: None` from `AgentConfig` literals in
   `tests/cli_driver.rs`. Finish with a repo grep proving only global-prompt
   `system_prompt` uses remain.
5. [x] Documentation (must land in the same commit as the code):
   - `docs/configuration.md`: the TOML agent-type example uses
     `persona = "…"`; replace "so `persona` is always prepended to the file
     body and also works when no file body exists" with the single-property
     rule (body is the persona in `.md`, frontmatter must not carry one, body
     required, inline `persona` *replaces* the file-sourced persona); collapse
     the profile-field table's `persona`/`system_prompt` rows into one
     `persona` row naming all three sources; collapse the initial
     system-prompt contract's "agent-type `persona`, agent-type file body"
     steps into one "agent-type `persona`" step and renumber; state that the
     persona extends the baseline rather than replacing it.
   - `docs/features.md`: agent-type authoring paragraphs — the Markdown body
     *is* the persona, inline `persona` overrides it (replacing the old
     "always precedes that body" wording); "Agent-type persona and file body
     are then appended" → the agent-type persona is appended as one section.
   - `docs/implementation-notes.md`: composition sentence ("`persona` and the
     file-authored `system_prompt` are composed after the global SOUL…") →
     single persona; add the parsing mechanics (standard-struct frontmatter,
     body overrides, body-mandatory bail-out, inline override replaces, compose
     pushes one section).
   - `CONTEXT.md`: **Persona** — state that an agent type's persona extends
     the global SOUL rather than replacing it.
6. [x] Final verification: `make format lint`, the mandatory gate
   `make test-coverage`, and `rumdl fmt` on touched Markdown files.

## Implementation Hints

Codebase facts:

- `src/config/agent.rs`: `AgentConfig` derives `Deserialize` and is used
  directly by `toml::from_str` for `.toml` agent files **and** for `.md`
  frontmatter (`toml`/`serde_yaml`), while inline `[agents.<type>]` tables
  deserialize into `AgentConfigOverrides` (`src/config/agent_type_file.rs`),
  which already exposes only `persona` among prompt fields.
- `src/config/agent_type_file.rs` → `parse_markdown`: frontmatter first
  (must open and close with `+++`/`---`), then
  `config.system_prompt = Some(body.trim().to_string())` unconditionally —
  today even for an empty body, which pushes an empty section.
- `src/config/profile.rs` → `load_agent_types`: whole-file definitions win
  per scope (project > profile > global), then
  `apply_agent_config_overrides` merges inline fields one by one;
  `AmbiguousAgentTypeDefinition` fires when an inline table names a type whose
  file lives in the *active* scope.
- `src/agent_factory.rs` → `compose_agent_system_prompt(global, agent_config)`
  pushes global, then `persona`, then `system_prompt`, joined with `"\n\n"`.
  `AgentFactory.system_prompt`/`AgentLoop.system_prompt` are the *global*
  prompt (`with_system_prompt`) and stay untouched.
- Test sites carrying the removed field: `src/config/agent_type_file.rs`
  (fixtures/assertions), `src/config/profile.rs` (defaults, inline
  initializer, file fixtures, override tests, literals), `src/agent_factory.rs`
  (literals + expected composed strings), `tests/cli_driver.rs` (literals).
- Docs defining the current contract: `docs/configuration.md` → *Global
  persona (`SOUL.md`)* (initial system-prompt contract) and *Agent-type
  files* (examples + profile-field table), `docs/features.md` → *Agent types*
  and *Layered global persona and initialization*,
  `docs/implementation-notes.md` → *Layered configuration and persona
  assembly*, `CONTEXT.md` → *Persona*.
- `docs/architecture-design-records/` contains only `.gitkeep`; no recorded
  decision constrains this change.
- Repo gates (`docs/development-workflow.md`): `make format`, `make lint`,
  the mandatory `make test-coverage`; Markdown checked with `rumdl`
  (`.rumdl.toml`, 100-column lines).
- Today's documented effective prompt order: global SOUL (+ trusted
  instruction sections), agent-type `persona`, agent-type file body, tool
  protocol, then (for a subagent) its briefing and runtime constraints. This
  change removes the "agent-type file body" position; nothing else moves.
