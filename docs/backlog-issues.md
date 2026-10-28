# Backlog of issues discovered while testing

- [x] Why was this `read` subject to user-approval-required ?
      `~/.agents/skills/graphify/references/query.md`
      Shouldn't have it been allowed by the read-rule `skill_sidecar` ?
      See `/Users/manuelkoch/.ylva/profiles/default/config.toml`.
      See logs at `tmp/tui_20261027221226.log`.
      Ah, I guess it is because the dir has ".agents" which is matched by the
      `dot_path` rule.
      Would it be better to place the `skill_sidecar` rule last to avoid this
      behaviour ?
      Yes, that would allow the intended behavior.
