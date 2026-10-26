# Slash commands in TUI

## /agent

A child conversation can already be opened by double-clicking its `spawn_subagent`
bubble; the proposed `/agent` command would add an aggregate run list, not the
basic drill-in. The implemented commands are documented in
[features](../features.md#slash-commands). Only the proposals below remain
deferred.

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

Should this be a dedicated keybinding instead of a slash-command ?
Keybinding would be more usable when you are in the mids of drafting a message
and want to view finished/running subagents or a current clarify/approval
is shown and pressing esc would abort the current clarify/approval.

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
