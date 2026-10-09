# Slash commands in TUI

The `/models`, `/tools`, and `/clear` commands are implemented. A child
conversation can already be opened by double-clicking its `spawn_subagent`
bubble; the proposed `/agent` command would add an aggregate run list, not the
basic drill-in. Session resume still uses explicit `--conversation-id`; there
is no `/session` picker. These additions remain deferred.

## /agent

Using `/agent` to show overlay of all (sub) agents that are avail in current
session and what their status is ( running / done / error ).
Picking one of the agents switches the view to that agents conversation-list,
scrolled down to the last item.

Order the entries in the agents popup list by descending priority:

- running: highest prio
- scheduled/pending: medium prio
- finished/error: low prio

For every agent show info:

- indicator whether this agent is the "current" agent ( the conversation
  user is currently viewing )
- title/label ( elided if too long, ensure the other info can be displayed )
- result (sucess/error), if avail
- elapsed run duration ( counting up while the run is in progress )

## /session

Show recent x sessions in a popup list to let other choose one to resume a session.

Display the following info for each session:

- date/time of last usage ( derived from the last message in conversation )
- title

Sort sessions by descending last-used timestamp ( i.e. newest first ).

## /compact

Trigger conversation compaction for current agent conversation.

## /status

Show the sources of the composed system prompt in a popup, so it is visible
where the model's instructions came from:

- current provider/engine
  - connection
  - engine
  - model
  - model parameters if available/configured ( reasoning effort, context-size,
    temperature etc )
- current agent type
  - configuration ( allowed tools )
- current context usage, grouped by origin; incl its estimated token contribution
  - system prompt
    - soul (the resolved `SOUL.md` layer path, workspace `.ylva/SOUL.md`,
      Ylva home `SOUL.md`, or the built-in value)
    - instructions (every trusted instruction file that was loaded,
      with its sentinel source e.g. `$YLVA_HOME/AGENTS.md`, `sub/AGENTS.md`)
    - tool schema description
  - tool calls ( the call and its result )
  - assistent messages
  - user messages
