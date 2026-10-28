# Backlog of issues discovered while testing

- [x] Exit code of non-zero on a bash tool run is not necessarily an error.
      We should not render the whole output of the bash run in the collapsed
      bash bubble in TUI. Rather just the first line that states the exitcode:
      "Tool 'bash' failed: command exited with code 1:" and only show the output
      when the bubble is expanded. And we should render the output in a neutral
      color instead of red.
