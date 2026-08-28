# Inter Agent Communication

When the main-agent starts sub-agent(s) it may be helpful that

- parent-agent can send steering message(s) to its child-agent(s)
- child-agent can send steering/feedback message(s) to its parent-agent

## New tools

We could introduce new tools for this communication, depending on the parent/child agent context.

## Identifying agent

The `spawn_agent` tool has a tool-id, that could be used to identify the sub-agent.
Or we assgn an internal agent-id and fed that back to llm, when `spawn_agent` is used
so that parent-agent and child-agent know their counter-parts and can the use their inter-agent-communication-tool calls with the proper IDs.
