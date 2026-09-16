# Backlog of issues discovered while testing

- [x] "init" creates an empty ".env" file in the "default" profile
      with a comment describing its use case.

- [x] "init --workspace" creates a ".gitignore" file in the workspace
      ".ylva" directory and excludes any ".env" files from git.

- [x] Unable to select text in subagent conversation header bubble.
      Trying to drag on the header just toggles expand/collapse but
      doesn't select nor copy selected text.

- [ ] We don't load past sessions to provide functionality to walk thru
      recent user messages to fill draft message input.
