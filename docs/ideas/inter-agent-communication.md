# Inter-agent communication

Deferred idea. The current human-to-root steer queue is implemented, but
agents cannot yet steer one another.

When the root agent starts subagents, it may be useful if:

- the parent can query for subagent progress ( conversation length, number of tool
  calls, current runtime duration, etc. ) to report back to user that it is still busy.
- the parent agent can send steering messages to its subagents
- a subagent can send steering/feedback messages to its parent agent
- a subagent can ask parent for guidance or missing information

## New tools

New tools could handle this communication, depending on the parent/subagent
context. They must preserve the current tool-call protocol: every issued tool
call receives exactly one result before later conversation messages are added.
For async communication we may need to use new system/user message to inject
into parent/subagent conversation.

## Identifying agents

The existing `spawn_subagent` lifecycle already assigns a `run_id` and
`tool_call_id`. Prefer the `run_id` to address a subagent. Do not add a second
identity unless the communication protocol requires one.
