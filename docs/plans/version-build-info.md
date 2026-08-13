# Version and Build Info

Update the binary build to include the following info

- current version, derived from `Cargo.toml`
- git commit hash of HEAD
- build timestamp, as an UTC timestamp "2026-08-13T16:45:00+Z"

## CLI

Update CLI to print this build info on demand
via a cli-flag (--version).

## TUI

Update TUI to show this build info on demand
via dedicated keybinding and popup about dialog.
