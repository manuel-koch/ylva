# Slash commands in TUI

Slash commands can be used to trigger advanced functionality,
configuration changes, or other actions in the TUI.

Slash commands will only be triggered when it is the first text in the 
user draft-message input field.

Slash commands can open an overlay ( covering the conversation-list ) that show further info while the user is typing ( e.g. showing auto complete options or help for the current typed command ).

Confirming ( sending ) a slash command will perform the action associated with that
command.

A command may generate a result message/bubble in the conversation list ( which may even be persisted ).

# /clear or /reset

Clear or resets current conversation, effectively starting a new fresh session.

# /models

Using /models to select the current provider and model.

# /agent

Using `/agent` to show overlay of all (sub) agents that are avail in current session and what their status is ( running / done / error ).
Picking one of the agents switches the view to that agents conversation-list,
scrolled down to the last item.
