# Slash commands in TUI

The implemented commands are documented in [features](../features.md#slash-commands).
Only the proposals below remain deferred.

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
