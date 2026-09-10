# Backlog of issues discovered while testing

- [x] /models command only allows completion for the command itself,
      typing an argument after the slash-command doesn't prefilter the
      list of available models

- [x] Typing "/model", models popup is shown with an entry selected,
      hitting enter sends "/model" to llm instead of activating the current selected entry in the popup.
      Can't type tab to autocomplete on the selected entry either.

- [x] "/models" popup should show the predefined "engines" first, as
      shortcuts to the provider/model/config that they use.
      Add a seperator between the "engines" section and the known
      "models" section.

- [x] "/models" popup shows verbose model name / title too for some models.
      This title is most of the time almost identical to the plain model-name.
      IMO we don't need that.
      Additionally that title make the line sometimes too long, so that important
      info like context-length and reasoning are clipped.

- [x] Ability to toggle reasoning-effort in the "/models" popup is rather hidden.
      It only works for some models. Maybe we should highlight whether the reasoning
      can be adjusted or not. Otherwise user tries `←`/`→` to cylce it, but
      nothing seems to happens.
      The keybindings in the footer don't give a hint about `←`/`→` or enter/esc.
      Footer still shows "enter: send" which is wrong while the slash-command
      popup is active.

- [x] Switching a model via "/models" slash-command persists a "system" message
      in conversation:
      ```json
      {
      "type": "message", "v": 1,
      "message": {
      "role": "system", "content": "Switched to selected model (reasoning: provider default)."
      }
      }
      ```
      Shouldn't this be just a notice that is rendered in TUI, but it seems to be
      a real "system" message in the conversation send to llm ?

- [x] "/models" should display the llm context-length as kb/mb size
      ( integer representation, 2048 == 2kb ) to increase readability.

- [ ] "/models" popup dialog should provide vertical scroll-indicators
      for showing engines & models.

- [ ] "/models" can't scroll by mouse in the popup dialog showing engines & models.

- [ ] Can't use mouse drag'n'drop to select text in slash-command popup.

- [ ] Can't use mouse click to select an entry in slash-command popup.

- [x] Can't use fn-up/down to page scroll in /models popup list of providers/models

- [x] Error on loading/parsing profile `config.toml` doesn't state which profile
      was tried ( neither the name nor the full directory ), only the config error
      is printed, like "error: profile references engine 'default', which no [engines.*] table defines"
