# Inter-agent communication

Deferred idea. The current human-to-root steer queue is implemented, but
agents cannot yet steer one another.

When the root agent starts subagents, it may be useful if:

- the parent agent can send steering messages to its child agents
- a child agent can send steering/feedback messages to its parent agent
- a child can ask for guidance or missing information
- a parent can receive progress before the child finishes

## New tools

New tools could handle this communication, depending on the parent/child agent
context. They must preserve the current tool-call protocol: every issued tool
call receives exactly one result before later conversation messages are added.

## Identifying agents

The existing `spawn_subagent` lifecycle already assigns a `run_id` and
`tool_call_id`. Prefer the `run_id` to address a subagent. Do not add a second
identity unless the communication protocol requires one.

## Related

- [`agent-steering.md`](./agent-steering.md) is retained as a pointer to this
  consolidated note.
