# Ylva Domain Language

Ylva is an agent-harness for model-driven conversations, controlled tools, and
delegated work. This glossary defines the canonical terms for that domain.

## Workspace and configuration

**Workspace root**:
The top-level directory that defines the active workspace for a Ylva run.
Workspace-local configuration and filesystem access are scoped to it.
_Avoid_: project root, repository root

**Ylva home**:
The per-user Ylva home that contains user-level configuration and persisted
history.
_Avoid_: home root

**Configuration layer**:
A scope at which Ylva provides a configuration resource: workspace, Ylva home,
or built-in.

**Built-in default**:
A code-owned fallback used when no workspace or Ylva home configuration
supplies a value.

**Trusted instruction file**:
An `AGENTS.md` or `CLAUDE.md` whose content is appended to the global baseline
of the system prompt. Which file names are trusted, where they are discovered,
and how they are composed is code-owned; arbitrary Markdown is never loaded.
_Avoid_: agents file, conventions file

**Profile**:
A named runtime configuration that selects engines and supplies settings for
agents.

**Agent type**:
A named specialization describing an agent's persona, tools, delegation
ability, and runtime settings.

**Root agent type selection**:
The agent type a root conversation runs as: an explicit `--agent` wins, a
resume otherwise continues with the type the session last stored, and a fresh
session starts as `default`.
_Avoid_: active agent, chosen persona

**Tool-use iteration limit**:
The per-agent-type cap (`max_tool_iterations`) on consecutive tool-use rounds
a single turn may spend; it defaults to 50 for root and subagent loops and a
value of zero is rejected.
_Avoid_: tool loop cap, max iterations

**Compaction prompt**:
The instruction framing a compaction summary request, resolved from
`COMPACTION.md` like the global SOUL (workspace, Ylva home, built-in) and
supplied to the compact engine.
_Avoid_: summary system prompt

## Agent identity and execution

**Global SOUL**:
The shared Ylva identity and behavioral baseline for every agent context. It is
separate from an agent type and is combined with trusted instruction content
and type-specific instructions.
_Avoid_: default system prompt

**Persona**:
Identity and behavioral guidance presented to a model. The global SOUL is the
shared persona; an agent type may add one specialization that extends the
global SOUL rather than replacing it.

**Agent role**:
The execution position of an agent: root for the user's conversation or
subagent for delegated work.

**Root agent**:
The agent that owns the user's active conversation and coordinates subagents.

**Subagent**:
An agent running a delegated task on behalf of a root agent. A subagent has its
own agent type and task briefing while remaining part of the root agent's work.
_Avoid_: child agent, child

**Briefing**:
Task-specific instructions and context supplied to a subagent by its parent.

**Context**:
The ordered model-facing message history belonging to one agent run.

**System prompt**:
The complete instruction message presented to a model for an agent context,
including the global SOUL with its trusted instruction content and any
applicable agent-type guidance.
_Avoid_: root agent's initial system message

**Transcript**:
The durable, ordered record of a conversation's messages as persisted and
replayed in the UI. Compaction never prunes it: every original message stays
browsable before and after a resume.
_Avoid_: history, chat log

**Model view**:
The ordered message list actually sent to a model for a request: the leading
system block, an optional compaction summary, and the verbatim tail after the
compaction boundary.
_Avoid_: prompt payload, request messages

**Tool**:
A named capability that an agent can invoke, such as reading files, editing
files, searching, running commands, spawning agents, or asking the user for clarification.

**Sandbox**:
The filesystem boundary that governs which paths tools may access and when user
approval is required.

**Token estimate**:
An approximate token count produced locally with `tiktoken-rs` `cl100k_base`.
The compaction budget estimates the model view and active tool definitions;
the TUI footer shows the serialized JSON request-body estimate during requests
and the model-view estimate after compaction changes the view. Serialized-body
estimates count that body as text. Neither estimate reproduces provider request
processing or the selected model's tokenizer.
An opt-in per-request diagnostic dump records this local estimate beside any
provider-reported prompt/input token count observed on the same request. The
provider count reflects the serving side's accounting when available. Local
estimates are advisory and are never billed usage.
_Avoid_: token count, usage

**Reported input-token count**:
A provider-reported prompt/input token count for one completed model request,
when the provider returns it. It is distinct from the local token estimate and
may be unavailable when usage is omitted or a stream is interrupted.
_Avoid_: billed usage, exact token estimate

**Context window**:
The active model's effective token capacity, resolved per engine from the
profile's `context_length`, then the provider catalog, then the built-in
default.
_Avoid_: max context, token limit

**Compaction**:
Replacing the oldest summarized part of the model view with a durable summary
so a long conversation keeps fitting its context window, while the transcript
stays intact and browsable.
_Avoid_: truncation, history compression

**Compaction boundary**:
The transcript index where the summarized prefix ends and the verbatim tail
begins. It never separates a tool call from its one matching result.
_Avoid_: cut point, split index

**Compaction thresholds**:
The per-engine usage fractions of the context window that decide when to act:
the soft threshold starts a background summary after a completed turn, the
hard threshold forces a synchronous drop instead of waiting for one.

**Per-agent compaction settings**:
An agent type may override the profile's compaction engine and summary timeout.
Each omitted override inherits independently. Root and child runs use the
settings resolved for their own agent type.

## Models and providers

**LLM**:
The language model selected to generate an agent's responses. Configuration and
provider APIs may refer to the selected LLM as a `model`.

**Provider**:
The service integration that communicates with a model.

**Provider connection**:
A named endpoint and credential configuration for one provider. Multiple
connections may use the same provider.

**Provider request concurrency cap**:
The maximum number of simultaneous requests sent through a provider connection;
it is shared by engines and agents using that connection.

**Engine**:
A reusable runtime selection that binds a model to a provider connection and
runtime settings.

## Persistence

**Conversation**:
The user-visible exchange owned by a root agent, including work performed by
delegated subagents.

**Session**:
The persisted record of one conversation run, including its messages and
delegation events.

**Conversation ID**:
The identifier used to reopen a persisted session for continuation.

**Session clone**:
A new session that replays another session's logical events under its own
session ID, linked back by `cloned_from`. The source file is only read — it
is never repaired, appended to, or rewritten — and all new writes go to the
clone. Only top-level sessions can be cloned.
_Avoid_: copy, fork, duplicate
