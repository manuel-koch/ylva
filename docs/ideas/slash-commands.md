# Slash commands in TUI

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
- runtime duration ( counting up if currently running )

## /session

Show recent x sessions in a popup list to let other choose one to resume a session.

Display the following info for each session:

- date/time of last usage ( derived from the last message in conversation )
- title

Sort sessions by descending last-used timestamp ( i.e. newest first ).
