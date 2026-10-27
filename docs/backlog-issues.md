# Backlog of issues discovered while testing

- [ ] Going out of a drilled-in subagent conversation using `esc` key can
      accidentially abort a tool approval or clarify that pops up just
      instantly before the user can actually recognize it.
      Maybe another keybinding would be better ?
      Maybe `ctrl+p` or `F2` or a dedicated `back to parent` button in the
      subagent header bubble ?

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
