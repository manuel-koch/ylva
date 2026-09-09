# Backlog of issues discovered while testing

- [ ] /models command only allows completion for the command itself,
      typing an argument after the slash-command doesn't prefilter the
      list of available models

- [ ] Can't use fn-up/down to page scroll in /models popup list of providers/models

- [x] Error on loading/parsing profile `config.toml` doesn't state which profile
      was tried ( neither the name nor the full directory ), only the config error
      is printed, like "error: profile references engine 'default', which no [engines.*] table defines"
