# Treat harmless shell redirections as no-ops in the `bash` command rules

## Context

Ylva's `bash` tool statically parses each command script
(`src/sandbox/bash_command_extractor.rs`, `tree-sitter-bash`) and evaluates every
extracted `command` against the `[tools.bash]` rule list (or the default floor). Any
construct outside the fixed scanning scope (`list`/`pipeline`/`command` nodes) makes the
statement `Unanalysable`, which yields `user_approval_required`. Redirections are in that
bucket: `git status 2>&1` or `cmd 2>/dev/null` is wrapped in a `redirected_statement` node
and therefore prompts the user, even when the command itself is auto-approved.

Observed problem: in a real session several read-only-like commands still required user
approval only because they carried trivial redirections (stderr-to-stdout, stderr-to-devnull).

Goal: recognise a **closed set of harmless redirections** and evaluate the wrapped command as
if the redirection were absent, so the verdict comes from the command itself. Redirections
to real file paths stay unanalysable (they are writes, e.g. `> ~/.ssh/authorized_keys`).
No new named rule is introduced: the safe set needs no policy knob. A user-configurable
`file_redirection` named rule for file targets is out of scope.

Harmless set (all must be literal and unquoted, with no expansion):

- fd duplication/closing between file descriptors: `N>&M`, `N<&M`, `>&N`, `N>&-` (digits or
  `-` only; `>&word` is a *file* redirect in bash and must stay unanalysable);
- output to the literal `/dev/null`: `>/dev/null`, `N>/dev/null`, `>>/dev/null`,
  `&>/dev/null`, `&>>/dev/null`;
- input from the literal `/dev/null`: `</dev/null`.

Acceptance criteria:

- `git status 2>&1`, `git log 2>/dev/null`, `cmd >/dev/null 2>&1`, `cmd </dev/null` and the
  same inside `|`/`&&`/`||` chains are evaluated as the bare command (auto-approved when the
  command is, denied when it is, etc.).
- `cmd > file`, `cmd >> file`, `cmd 2> file`, `cmd &> file`, `cmd >&file`, `cmd > $VAR`,
  `cmd > "/dev/null"` is still decided as before (`user_approval_required`), heredocs and
  here-strings (`<<`, `<<<`), process substitution and `/dev/nullx`/`../dev/null` variants
  likewise.
- A mix of harmless and non-harmless redirections on one command stays unanalysable.
- Docs describe the new behaviour; coverage threshold still met.

## Implementation Plan

Implementing agent: flip `[ ]` to `[x]` immediately after completing each step and rewrite
this file in place so progress is resumable across sessions.

1. [ ] Read `docs/development-workflow.md` (*Plan execution*, testing, license headers) and
   `docs/known-test-issues.md`; follow TDD (tests first for steps 2–3).
2. [ ] In `src/sandbox/bash_command_extractor.rs`, add a grammar-fact test (in the existing
   `grammar` test module at the end of the file) that dumps/asserts the `tree-sitter-bash`
   node shape for `git status 2>&1`, `cmd >/dev/null`, `cmd &>/dev/null`, `cmd </dev/null`,
   `cmd >&2`, `cmd >&file`, `cmd > file` and a piped form (`cmd 2>&1 | grep x`): the
   `redirected_statement` node with its `body`/`redirect` fields, `file_redirect`
   `descriptor`/`destination` fields, and the operator token kinds. Record the confirmed
   facts in the module doc "Grammar facts" list (replacing the current bullet that says a
   redirect always makes the statement un-analysable).
3. [ ] Add failing extractor unit tests (next to `heredoc_is_unanalysable`) for every case in
   the acceptance criteria above, including inside pipelines and `&&`/`||` lists and a
   trailing `&` (backgrounding must still be flagged on the unwrapped command).
4. [ ] Implement the unwrap: in `collect_statement`, handle `redirected_statement` by
   checking every redirect child with a small helper (e.g. `is_harmless_redirect`) and, only
   if all are harmless and the `body` is a supported node, recursing into the body;
   otherwise push `ExtractedStatement::Unanalysable`. Keep the helper strict: operator must
   be in the closed set, destination a bare unquoted `word`/`number` node exactly equal to
   `/dev/null` (or digits/`-` for fd duplication). Use descriptive names (no
   abbreviations).
5. [ ] Check `src/sandbox/command.rs` / `src/sandbox/command/tests.rs`: add an end-to-end
   test that `git status 2>&1` and `git log 2>/dev/null` get the same verdict as without the
   redirection under the default floor and under an explicit `{ command = [...] }` rule, and
   that `git status > out.txt` is still `UserApprovalRequired`.
6. [ ] Update the doc comments in `src/sandbox/command.rs` (the "Extraction" paragraph
   mentioning redirections) and the module docs in `bash_command_extractor.rs` (scanning
   scope paragraph and `ExtractedStatement::Unanalysable` doc).
7. [ ] Update `docs/configuration.md` (*`bash` command rules*, the paragraph listing
   unsupported constructs) to state that fd duplication and redirection to/from the literal
   `/dev/null` are ignored for rule matching while all other redirections remain
   `user_approval_required`. Update `docs/implementation-notes.md` if it describes the
   extractor's scope (grep first); update `docs/features.md` only if it describes bash
   approval behaviour.
8. [ ] Re-run the offline replay (`tests/sandbox_evaluation/README.md`, `make` target for
   the sample report) against the corpus
   `tests/sandbox_evaluation/data/copilot-bash-commands-1792598338.json` and compare the
   verdict diff to the committed `*.report.json` files; only commands containing harmless
   redirections should change. Regenerate the committed reports only if the repository
   convention requires it (ask the user first).
9. [ ] Run `rumdl fmt` on changed markdown, then `make format lint` and
   `make test-coverage` (mandatory gate; no dependency change, so no `make audit`).
10. [ ] Mark this plan's steps done and, per *Plan execution*, update any plan statement that
    turned out to be out of date.

## Proposed Improvements

- Add an opt-in named rule `file_redirection` (default `user_approval_required`, deniable)
  for redirections to real file paths, optionally combined with the path rules
  (`outside_workspace`, `dot_path`) so `> ./target/out.log` can be auto-approved by users
  who want it. Not part of this change.
- Surface a hint in the approval prompt when a command needed approval only because of an
  unsupported construct, to make this class of friction easier to diagnose.

## Implementation Hints

- Extraction scope today: `collect_statement` in `src/sandbox/bash_command_extractor.rs`
  walks only `command`, `list` and `pipeline`; every other node kind (including
  `redirected_statement`) becomes `ExtractedStatement::Unanalysable`.
- `collect_top_level_statements` sets `backgrounded` on the `ExtractedCommand`s produced
  since the last `;`/`&`, so an unwrapped command inside a `redirected_statement` is
  flagged correctly provided it is pushed via the same `out` vector.
- The extractor's module doc explicitly states that a redirect wraps the `command` in a
  `redirected_statement` sibling node (not a child of `command`); the grammar-fact tests in
  the `grammar` submodule at the bottom of the file are the project's pattern for pinning
  `tree-sitter-bash` v0.25.1 behaviour. Field names (`body`, `redirect`, `descriptor`,
  `destination`) and operator token kinds are expected from the grammar but must be
  confirmed by step 2 before implementing.
- `SandboxContext::check_command` (`src/sandbox/command.rs`) maps `Unanalysable` to
  `UserApprovalRequired`; extractor changes need no change to rule evaluation.
- Docs to touch: `docs/configuration.md` ("`bash` command rules", unsupported-constructs
  paragraph, ~line 1205). No ADR in `docs/architecture-design-records/` covers the bash
  sandbox (0001 git conventions, 0002 release automation, 0003 workspace trust).
- Offline replay tooling: `tests/sandbox_evaluation/` (`bash_policy_replay.rs`, `Makefile`,
  `README.md`, `sample.config.toml`, recorded corpus under `data/`). Recorded commands are
  never executed.
- Bash semantics to respect: `>&word` and `&>word` redirect to a *file* named `word`; only
  digits or `-` after `>&`/`<&` mean fd duplication/closing. `/dev/null` must match exactly
  (no `/dev/nullx`, no quoting, no expansion, no path normalisation).
- Project conventions: Apache-2.0 license headers on new files, full descriptive names,
  file-per-module (`foo.rs`), docs updated in the same commit, tests per
  `docs/development-workflow.md`.
