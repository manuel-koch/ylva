# Plan: `ylva profile` CLI command

Source idea: [docs/ideas/profile-cli-command.md](../ideas/profile-cli-command.md)

## Context

### Problem

Profiles are only created by hand or by `ylva init` (which seeds
`profiles/default/` only). Users have no CLI to list, create, delete, or edit
profiles, so they must know the on-disk layout under `YLVA_HOME` and the
`config.toml` / `.env` file names.

### Goal

Add a provider-independent `ylva profile` subcommand family:

- `ylva profile list`: show profiles under the Ylva home and the current
  workspace (merged, workspace first); `--workspace` lists only the workspace's.
- `ylva profile create <name>`: create a profile seeded with the same starter
  files `ylva init` writes for `default`.
- `ylva profile delete <name>`: delete a profile directory and all its files;
  `--dry-run` previews.
- `ylva profile edit <name>`: open the profile's `config.toml` in an editor.

Shims (`ylva-<name>` wrapper scripts) are created and pruned by the shim tasks
below; templates are under [Shim templates](#shim-templates).

### Current state (relevant code)

- `src/cli.rs`: `CliCommand` has `Session { command }` and `Init { workspace, force }`.
  `SessionCommand` is the precedent for a nested subcommand group.
- `src/main.rs`: `Init` is dispatched before logging/color init; `Session` is
  dispatched after logging init and before profile loading. Profile
  subcommands are provider-independent and follow the `Session` placement.
- `src/session_command.rs`: precedent for a testable entry point:
  `run(command, output, diagnostics) -> anyhow::Result<u8>` plus
  `run_for_project(...)` with injected paths and writers.
- `src/init_command.rs`: `DEFAULT_PROFILE_STARTER`,
  `DEFAULT_PROFILE_ENV_STARTER`, `seed_file`, `InitReport`. Starter contents
  and seeding are reusable for `profile create`.
- `src/config.rs`: `default_profiles_root()` (`$YLVA_HOME/profiles`, else
  `~/.ylva/profiles`), `profile_config_path`, `profile_env_path`,
  `PROFILE_CONFIG_FILE_NAME`, `PROFILE_ENV_FILE_NAME`, `DEFAULT_PROFILE_NAME`.
- `src/config/profile.rs`: `resolve_profile_config_file` layers workspace
  `.ylva/profiles/<name>/` over home. Profile subcommands do not layer; they
  target either the home or, with `--workspace`, `<workspace>/.ylva/profiles/`.
- `src/config/layered.rs`: `discover_workspace_root(start)` returns the nearest
  ancestor containing `.ylva/` or `.git`. `init --workspace` falls back to the
  cwd when none is found; profile subcommands exit `2` instead.
- `src/exit_code.rs`: `Success=0`, `GeneralError=1`, `UsageError=2`,
  `ApprovalDenied=3`, `Interrupted=130`. The `--help` epilog is generated from it.
- No existing code reads `EDITOR` or `VISUAL`.
- Tests: `tests/cli_init.rs` and `tests/cli_profile.rs` spawn the binary
  with `env_clear()`, a preserved `PATH`, and `YLVA_HOME` set to a tempdir.

### Constraints

- AGENTS.md: full descriptive names; `main.rs` stays thin; `foo.rs` module
  files, not `foo/mod.rs`; integration tests reach items only via `src/lib.rs`.
- Docs are part of the change set (see the Documentation task under
  [Implementation Plan](#implementation-plan)).
- Verification gate: `make format lint`, then `make test-coverage` (≥ 80% lines).
- No ADRs exist in `docs/architecture-design-records/`, so none constrain this change.

### Non-goals (this plan)

- Rename, copy, or import of profiles.
- Creating or editing agent-type files under `profiles/<name>/agents/`.
- Validating engine or provider references during `create`.

## Implementation Plan

Tasks are ordered. Each task is complete only when its tests pass and its
docs are updated (see the Documentation task).

1. [x] **Add the profile name rule** in `src/config/profile.rs`.
    - Add `pub fn is_valid_profile_name(name: &str) -> bool`. Rule:
      `^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$`. Rejects path separators, `.`, `..`, empty
      names, and names that do not start with a letter or digit.
    - Pure function: no filesystem or environment access.
    - Unit tests in `src/config/profile/tests/`: accepted (`default`, `work-2`, `a_b`,
      a 64-char name); rejected (`""`, `-x`, `_x`, `../x`, `a/b`, `.`, `..`,
      `has space`, `my.profile`, a 65-char name).

2. [x] **Apply the rule to `--profile`** in `src/cli.rs`. This closes the pre-existing
   `--profile ../x` path traversal.
    - Add `value_parser = parse_profile_name` to `--profile`. `parse_profile_name`
      sits next to `parse_log_level`, wraps `is_valid_profile_name`, and returns an
      error that names the rule.
    - Invalid names become clap usage errors (exit `2`) before dispatch, so
      `--profile ../x` never reaches path construction. This also applies when a
      profile subcommand is given, because the value is still parsed.
    - Behavior change: a profile directory whose name fails the rule (for example
      `my.profile` or `work profile`) can no longer be selected with `--profile`.
      `profile list` marks it `(invalid name)`; renaming the directory restores access.
    - Tests in `tests/cli_profile.rs`: `--profile ../x session list` and
      `--profile a/b session list` exit `2` and name the flag; a valid name still
      resolves. Verify the existing `--profile` names in `tests/cli_profile.rs` and
      `tests/session_commands.rs` pass the rule.

3. [x] **Add CLI surface** in `src/cli.rs`.
    - Add `CliCommand::Profile { command: ProfileCommand }` with a doc comment.
    - Add `ProfileCommand::{ List, Create { name }, Delete { name }, Edit { name } }`
      with clap doc comments. Each action except `List` takes `name`, parsed with
      `parse_profile_name`.
    - Add `#[arg(long)] workspace: bool` to all four actions. It selects
      `<workspace>/.ylva/profiles/` instead of the Ylva home.
    - Add `#[arg(long)] force: bool` to `Create` and `#[arg(long)] dry_run: bool` to `Delete`.
    - `--shim-name` and `Shims` are added by the shim wiring task.
    - Add inline parse tests next to `init_parses_as_a_subcommand`.

4. [x] **Refactor starter seeding** in `src/init_command.rs`.
    - Extract a helper that writes the `config.toml` starter (`DEFAULT_PROFILE_STARTER`)
      and the `.env` starter (`DEFAULT_PROFILE_ENV_STARTER`) into an arbitrary profile
      directory, reusing `seed_file` and `InitReport`.
    - Extract the `.ylva/.gitignore` seeding (`WORKSPACE_GITIGNORE_ENTRIES`) into a
      helper that `create --workspace` can call.
    - `run_init` and `run_init_workspace` call the helpers. Their output and behavior
      must not change (`tests/cli_init.rs` must stay green unchanged).

5. [x] **Implement `list`, `create`, `delete`** in `src/profile_command.rs`.
    - Scope: without `--workspace`, the Ylva home (`default_profiles_root()`). With
      `--workspace`, `<workspace>/.ylva/profiles/`, where the root is
      `discover_workspace_root(cwd)`. No root → exit `2`; unlike `init --workspace`,
      there is no cwd fallback. Without `--workspace`, the workspace is never read or
      written.
    - Entry points: `run(command, output, diagnostics) -> anyhow::Result<u8>` and
      `run_for_profiles_root(command, profiles_root, ...)`, with injected root and
      writers, mirroring `session_command`. The thin `run` resolves the root.
    - `list`: sorted profile directory names with their path, one per line. No
      profiles → message, exit `0`. Read-only. Marks: `(no config.toml)` when missing;
      `(invalid name)` for directories failing the name rule; `(invalid config)` when
      `config.toml` cannot be read or fails to parse as TOML. Exit stays `0` for all marks.
    - `create <name>`: creates `profiles/<name>/`. Writes both starters only when
      neither exists. Does not create `agents/`. If `config.toml` or `.env` exists →
      exit `2` and change nothing, unless `--force`. `--force` overwrites those two
      files only; other files (for example `agents/`) are kept. An existing
      `config.toml` that is unreadable or fails to parse exits `2` before any write,
      even with `--force`; the user fixes it by hand. With `--workspace`, also seed a
      missing `.ylva/.gitignore` from `WORKSPACE_GITIGNORE_ENTRIES` through the
      seeding helper that `init --workspace` uses. An existing `.gitignore` is never
      overwritten, even with `--force`; its missing entries are appended.
    - `delete <name>`: removes `profiles/<name>/` recursively. No prompt and no
      `--yes`, same as `session delete`. Prints `Deleted profile "<name>" at <directory>`.
      `--dry-run` prints `would delete "<name>" at <directory>` and
      changes nothing. Missing profile → exit `2`. An unreadable or unparsable target
      `config.toml` → exit `2`, nothing removed. `delete default` is allowed; the
      implicit default then falls back to the built-in default (documented in the
      Documentation task). Shim pruning is added by the shim wiring task.
    - Exit codes: usage, invalid name, existing without `--force`, missing, no
      workspace root, and I/O failure → `2`.
    - Unit tests in an owner-local `#[cfg(test)] mod tests` using `tempfile`:
      create writes both starters into an empty profile; create with an existing
      starter fails unless `--force`, which overwrites only those two files; create
      keeps `agents/`; create on an unreadable or invalid `config.toml` exits `2`
      with no change, even with `--force`; create with `--workspace` seeds a missing
      `.ylva/.gitignore` and appends missing entries to an existing one; delete removes the tree;
      `--dry-run` changes nothing; delete of a missing name exits `2`; delete with an
      unreadable or invalid target `config.toml` exits `2` and removes nothing;
      `delete default` works; list sorted; list marks `(no config.toml)`,
      `(invalid name)`, and `(invalid config)` with exit `0`; list handles an empty
      root; `--workspace` lists only workspace profiles; `--workspace` without a
      root → exit `2`.

6. [x] **Implement `edit`** with an injected editor resolver and launcher.
    - Requires `profiles/<name>/config.toml`. Missing → exit `2` with a hint to run `create`.
    - Editor: `$VISUAL`, then `$EDITOR`. If neither is set or both are empty → exit `2`,
      naming both variables.
    - Launcher runs `sh -c '<editor> "$1"' sh <path>`, so values with arguments (for
      example `code --wait`) work. Non-zero editor exit → `1`.
    - Resolver: `(visual, editor)` values passed in, so unit tests never mutate process
      env (see `docs/known-test-issues.md`). Launcher injectable for tests.
    - Shim reconcile after a successful edit is added by the shim wiring task.
    - Unit tests: VISUAL beats EDITOR; EDITOR alone works; neither → exit `2`; missing
      `config.toml` → exit `2`; launcher non-zero → exit `1`.

7. [x] **Wire the module and dispatch.**
    - `src/lib.rs`: `pub mod profile_command;`. Re-export only what integration
      tests need.
    - `src/main.rs`: dispatch `CliCommand::Profile` after logging init and
      before profile loading, next to `Session`. Keep `main.rs` thin: only
      forward to `profile_command::run`.
    - `--profile` given together with a profile subcommand is ignored (as for `init`).

8. [x] **Integration tests** in `tests/cli_profile_subcommand.rs`.
    - Spawn `env!("CARGO_BIN_EXE_ylva")` with `env_clear()`, preserved `PATH`,
      and `YLVA_HOME` set to a tempdir.
    - Cover: `profile list` on empty and populated homes; `create` then file
      contents match the `init` starters; duplicate create → exit `2`;
      `create --force` overwrites only the two starters; `delete` removes the
      directory; `delete --dry-run` keeps it; `delete default` works; `--workspace`
      create, list, and delete in a tempdir containing `.git`; `--workspace` without
      a root → exit `2`; `edit` with `EDITOR=true` → exit `0`; `edit` with no editor →
      exit `2`; invalid name → exit `2`; `--profile x profile create y` creates `y`.

9. [x] **Check help snapshot.** Read `tests/cli_help.rs`. If it asserts the
   subcommand list, update it to include `profile`.
10. [x] **Shim claims and reconcile** in `src/profile_command.rs` (or `src/profile_shim.rs`
   if it grows).
    - Shim name: `ylva-<name>` by default. A top-level `shim_name` key in the
      profile's `config.toml` overrides it; an absent key means `ylva-<name>`. The
      value must pass `is_valid_profile_name` and must not equal `ylva`.
    - Ownership comes from the `config.toml` claim, not from the script text.
    - `default` gets no shim, so `ylva-default` is pruned if present. A `shim_name`
      in `default`'s `config.toml` aborts the reconcile.
    - One claim table per scope: home → `$YLVA_HOME/shims/` from `profiles/`;
      workspace → `<ws>/.ylva/shims/` from `<ws>/.ylva/profiles/`. A home and a
      workspace profile with the same name are not refused or suffixed; the user's
      `PATH` order decides which shim runs. A deleted workspace needs no cleanup,
      because its shims live inside it.
    - Build the full plan (claims, duplicates, prune list) before any write or
      removal. Abort with exit `2`, changing nothing, on: duplicate claims (naming
      both profiles), an unreadable or invalid `config.toml` in any in-scope
      profile, an invalid `shim_name`, `shim_name = "ylva"`, or a `shim_name` in
      `default`.
    - Write each claimed shim (mode `0755`, templates below). Prune regular files
      in the shims dir that no in-scope profile claims. Never touch subdirectories.
      Ylva owns the shims dir, so users must not place their own files there.
    - Tests with `tempfile`: claim table; duplicate abort with no change;
      bad-config abort with no change; prune only regular files; subdirectory
      untouched; `default` gets no shim and `ylva-default` is pruned; missing
      `config.toml` claims `ylva-<name>`; `shim_name = "ylva"` aborts.
11. [x] **Wire shims into commands.**
    - Add `ProfileCommand::Shims { workspace: bool }` and `--shim-name` on `create`
      to `src/cli.rs`.
    - `create --shim-name <name>` validates the name before any write, then writes
      `shim_name = "<name>"` as a top-level key before any `[table]` in the starter
      `config.toml`. An invalid name or `ylva` → exit `2`, no write.
    - `create`, `edit` (after a successful editor run), `delete`, and `profile shims`
      run the reconcile for their scope. `list` and `--dry-run` never reconcile.
    - `edit`: if the saved `config.toml` is invalid, the reconcile aborts with exit
      `2` and changes no shim.
    - `delete` validates the whole scope, target included, before any change; then
      it removes the target directory and reconciles, which prunes its shim.
    - Consequence: once shims are wired, `create`, `edit`, `delete`, and
      `profile shims` fail on any broken config in scope, not only the target. The
      checks in tasks 5 and 6 cover only the target.
    - Non-unix: `profile shims` exits `2`; the other subcommands skip shim work.
12. [x] **PATH hint** in `profile shims`.
    - Prints on stdout, after the shim list, as `#` comment lines:
      `# Update your PATH to use shims:`, then
      `# export PATH="<absolute shims dir>:$PATH"`. If the directory contains
      characters special to the shell, single-quote it instead.
    - The last line is `# <dir> is already on PATH` or `# <dir> is not on PATH`.
    - `profile create` prints the same hint after its `Shim <path>` line, but only
      when it created a shim for the profile.
    - Shim rows are `<profile>  <shim path>`, aligned like `profile list`, with no
      headings.
    - No shims in scope → no hint.
    - Read `PATH` only in the thin `run` entry point.
    - Tests: dir missing from `PATH` gives the `export` line; dir on `PATH` gives a
      confirmation line; no shims gives no hint; a dir with a space gets the
      single-quoted form.
13. [x] **Integration tests** in `tests/cli_profile_shims.rs`.
    - The generated home shim runs `ylva` with the right `--profile` and
      `YLVA_HOME`; quoted forwarded arguments (for example a value with spaces)
      survive.
    - The workspace shim `cd`s to the workspace root.
    - `ylva --profile x session list` and `ylva --profile x init` run through a
      shim (verify this assumption). A second `--profile` in the forwarded
      arguments exits `2`.
    - A custom `shim_name` survives `profile shims`; `create --shim-name ylva`
      exits `2`.
    - `edit` with `EDITOR=true` reconciles after the editor exits.
    - `list` changes no shim file.
    - `delete` of a profile with a broken `config.toml` exits `2` and removes
      nothing, `--dry-run` included. A broken config in another in-scope profile
      also exits `2`. A valid delete leaves no shim behind.
    - Duplicate claims exit `2` with no changes.
    - A `config.toml` with `shim_name` still loads (no `deny_unknown_fields`).
14. [x] **Mark the implicit default in `profile list`** in `src/profile_command.rs`.
    - The implicit default is the profile used when no `--profile` flag is given:
      `DEFAULT_PROFILE_NAME` (`default`). Its name is fixed in code, so the mark labels
      the profile; it is not a setting.
    - In `list` (both scopes), append `(implicit default)` to the row whose directory
      name is `DEFAULT_PROFILE_NAME`. Keep the task 5 marks. Each mark is its own
      parenthesized tag, with `(implicit default)` first.
    - If `profiles/default/` does not exist, no row carries the mark, because the
      built-in default needs no directory.
    - `list` stays read-only and exits `0`. Output is otherwise unchanged.
    - Unit tests: the `default` row is marked in the home and workspace scopes; no
      mark when `default` is absent; a `default` without `config.toml` shows
      `(implicit default) (no config.toml)`.
    - Integration test in `tests/cli_profile_subcommand.rs`: `ylva profile list` shows
      `(implicit default)` on the `default` line and on no other line.

15. [x] **Documentation** (part of the same change; not optional).
    - `docs/cli-options.md`:
      - `--profile` row: the name rule; invalid names exit `2`.
      - A `profile` section: the four actions, `--workspace`, `--force`, `--dry-run`,
        and `profile shims` with its PATH hint. Document the list marks, including
        `(implicit default)`.
      - Exit codes for `profile`, and the exit-code examples row for `2`.
      - `profile --workspace` exits `2` without a workspace root, unlike
        `init --workspace`, which falls back to the cwd.
      - Reword the "global flags" sentence. First list the flags that carry
        `global = true` in `src/cli.rs`, then state that `--profile` is top-level and
        goes before the subcommand. Check which subcommands ignore it in `src/main.rs`
        before saying so.
    - `docs/configuration.md`, "Profile file": the `profile` actions; the `VISUAL` /
      `EDITOR` lookup; `delete default` falling back to the built-in default; the
      `shim_name` key; the Ylva-owned shims dir; symlinked shims are unsupported; each
      workspace's `.ylva/shims` must be on `PATH` separately; the POSIX `export` line
      and the POSIX `export` line.
    - `docs/features.md`: entries for profile management and shims.
    - `docs/implementation-notes.md`: `profile_command.rs` and the shim module.
    - `README.md`: update only if it lists commands. Check first.
    - `docs/known-test-issues.md`: only if a new test quirk appears.
    - `CONTEXT.md`: no change expected; "profile" is already defined.
    - Run `rumdl fmt` and `rumdl check` on every changed Markdown file.

16. [x] **Verify.** Run `make format lint`, then `make test-coverage`. Do not run `make test` separately.

### Shim templates

Shims call `ylva` from `PATH` and place `--profile '<name>'` before the forwarded
arguments. Profile names pass the name rule, so the single quotes are safe.
Symlinked shims are unsupported, because a workspace shim derives the workspace
root from `$0`. Unix only. Home shims set no environment: they inherit the
caller's `YLVA_HOME`, so they store chats where plain `ylva --profile <name>`
does in the same shell.

Home shim `ylva-<name>` in `$YLVA_HOME/shims/`:

```sh
#!/bin/sh
exec ylva --profile '<name>' "$@"
```

Workspace shim `ylva-<name>` in `<workspace>/.ylva/shims/`:

```sh
#!/bin/sh
cd "$(dirname "$0")/../.." || exit 2
exec ylva --profile '<name>' "$@"
```

The workspace shim `cd`s to the workspace root first, because profile lookup is
cwd-based. Ylva does not edit shell rc files.

## Implementation Hints

- Keep `profile_command.rs` free of `std::env` reads in the logic path. Pass
  `VISUAL` and `EDITOR` in, and read them only in the thin `run` entry point.
- Resolve the workspace root only in the thin `run` entry point, never inside the logic.
- Use `tempfile::TempDir` for all filesystem tests.
- Use `YLVA_HOME` in integration tests so nothing touches the real `~/.ylva`.
- Reuse `seed_file` so write-if-missing and parent-directory creation behave
  the same as `init`.
- Starter contents must come from `init_command` constants, not be duplicated.
- Build the full reconcile plan (claims, duplicates, prune list) before writing
  or removing any file. A failure in validation then leaves the disk unchanged.
