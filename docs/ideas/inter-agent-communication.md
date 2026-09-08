# Inter-agent communication

When the main agent starts sub-agents, it may be useful if:

- the parent agent can send steering messages to its child agents
- a child agent can send steering/feedback messages to its parent agent

## New tools

New tools could handle this communication, depending on the parent/child agent context.

## Identifying agents

The `spawn_agent` tool has a tool-id that could identify the sub-agent.
Or we assign an internal agent-id and feed that back to the LLM when `spawn_agent` is used,
so that parent-agent and child-agent know their counterparts and can use their inter-agent
communication tool calls with the proper IDs.
