# Backlog of issues discovered while testing

- [ ] Slash-command "/models" doesn't search for the engine name
      when filtering matching provider/model from catalog and known engines,
      it just filters on provider/model name.

- [ ] Why is "openrouter-dense → openrouter/openai/gpt-6-luna context: 96kb
      reasoning: max ⚙ [IF]" incompatible with compaction engine
      selected by tests/e2e/custom/profiles/default/config.toml ?
      Context size is both 96kb, but the compaction-footer-line is orange.
      When I confirm the selected model, I get the confirmation notice
      "Switched to engine 'openrouter-dense' (openrouter/openai/gpt-6-luna). Compaction remains on 'local-dense'.", but there is no warning wrt context-size mismatch ( correct ).
