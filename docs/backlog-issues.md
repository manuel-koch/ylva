# Backlog of issues discovered while testing

- [ ] Switching provider/model via "/models" slash-command.
      Afterwards the TUI footer still shows an engine that is
      using the same provider-connection of the provider/model I picked.
      I have chosen a selected model from that provider-connection and
      not that configured engine though.
      The "engine" shown in the footer is misleading. It should show the
      name of the provider-connection if the selected provider/model
      doesn't exactly match a configured engine!

- [ ] On a narrow terminal, entering a long one-line text into the draft message.
      WHen cursor is at the 2nd rendered row of the text, pressing up-key to move to
      the first row performs prev-message-history functionality instead of moving the
      cursor.
      Message-history functionality should only trigger when at the visually rendered
      first/last row of the text, not when it is the unwrapped first/last row !
