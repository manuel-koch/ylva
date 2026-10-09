# Backlog of issues discovered while testing

- [x] Slash-command "/models" searches engine names as well as provider
      connections and model IDs. Multi-word filters match case-insensitively
      across those fields; engine names match only named-engine shortcuts.

- [x] `/models` compaction detail no longer turns orange solely because the
      highlighted engine uses a different provider connection or model. Equal
      effective context windows (including `openrouter-dense` and `local-dense`
      at `96kb` in `tests/e2e/custom/profiles/default/config.toml`) retain the
      normal detail color. Fixed compaction engines with different effective
      context windows still render orange. Catalog selections of another model
      use that model's window, not the configured model's context override.
      Explicit compaction engines keep their configured model and window even
      when the conversation switches models through the same named engine.
      Implicit compaction follows the selected conversation model and window.
