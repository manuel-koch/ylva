# Backlog of issues discovered while testing

- [ ] Triggering a tool call in LLM that requires user approval.
      Before the approval is requested, start typing a slash-command `/mod`
      in the message-draft, but don't complete it via tab/enter.
      When the tool approval request arrives, the rendering is a
      mixture of the slash-command-list and the tool-approval.
      We should hide any current open popup before rendering the
      tool-approval.
      When tool-approval is done, we should continue with the current
      draft-message ( which could be a slash-command )
