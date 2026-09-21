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

**Profile**:
A named runtime configuration that selects engines and supplies settings for
agents.

**Agent type**:
A named specialization describing an agent's persona, tools, delegation
ability, and runtime settings.

## Agent identity and execution

**Global SOUL**:
The shared Ylva identity and behavioral baseline for every agent context. It is
separate from an agent type and is combined with type-specific instructions.
_Avoid_: default system prompt

**Persona**:
Identity and behavioral guidance presented to a model. The global SOUL is the
shared persona; an agent type may add a specialization.

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
including the global SOUL and any applicable agent-type guidance.
_Avoid_: root agent's initial system message

**Tool**:
A named capability that an agent can invoke, such as reading files, editing
files, searching, running commands, or asking the user for clarification.

**Sandbox**:
The filesystem boundary that governs which paths tools may access and when user
approval is required.

## Models and providers

**LLM**:
The language model selected to generate an agent's responses. Configuration and
provider APIs may refer to the selected LLM as a `model`.

**Provider**:
The service integration that communicates with a model.

**Provider connection**:
A named endpoint and credential configuration for one provider. Multiple
connections may use the same provider.

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
