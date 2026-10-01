# Backlog of issues discovered while testing

- [x] Triggering a tool call in LLM that requires user approval.
      Before the approval is requested, start typing a slash-command `/mod`
      in the message-draft, but don't complete it via tab/enter.
      When the tool approval request arrives, the rendering is a
      mixture of the slash-command-list and the tool-approval.
      We should hide any current open popup before rendering the
      tool-approval.
      When tool-approval is done, we should continue with the current
      draft-message ( which could be a slash-command )
      Fixed: arming the approval hides the slash overlay, the
      per-action recompute keeps it hidden while the approval is
      pending, and resolving the approval restores the draft and
      reopens the overlay.
