# Profile CLI command

We should add a new subcommand `profile` that is related to profile
functionality:

## profile list

Using `ylva profile list` to show all available profiles.

## profile create

Using `ylva profile create <name>` to create a new profile and populate it with
initial configuration ( like `ylva init` does for the `default` profile ).

## profile delete

Using `ylva profile delete <name>` to delete an existing profile and all its configuration.

## profile edit

Using `ylva profile edit <name>` to start an editor ( using standard envvars
like `EDITOR`, research more known editor envvars ) for the profile `config.toml`.

## profile shims

We could add a shims directory in ylvas home to calling ylva with a selected profile.
For every profile we create a shell script that already includes the `--profile <name>`
options. The shim script would be called `ylva-<name>` or a custom script name.
Use can extend the `$PATH` variable to include the shims directory and just use
the shim script everywhere.

Shim script could even set YLVA_HOME based on its own location, thus using such
a shim script ( when it is avail in path ) can be called from anywhere
and still provide quick access to proper ylva home and related its profiles.

This shim approach requires any ylva command to accept/ignore the `--profile <name>`
option though or the shim only uses `--profile <name>` when no subcommand is provided
( e.g. `ylva-foo session ...` wouldn't get it, but `ylvy-foo` would ).

The shim script would pass any additional caller arguments unconditionally to the
invoked `ylva` executable ( including proper quoting of the arguments ).
