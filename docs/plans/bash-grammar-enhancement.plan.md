# Bash grammar enhancement: `sed`, more `git` subcommands, `grep` option placement

## Context

Ylva's `bash` tool auto-approves commands through ordered rules in
`[tools.bash].rules`. A rule with `only_arguments` / `with_arguments` is only
allowed for commands with a built-in, static **argument grammar**
(`src/sandbox/command_arguments*`). Commands without a grammar can only be
approved by an unconditional `{ command = ["x", "**"] }` rule, which approves
every option and operand. Recognition of an option is never approval; the user's
allow-list decides.

Goal: add grammars so frequently used, read-only command shapes can be approved
by narrow allow-lists instead of broad rules or repeated prompts, without
weakening the policy (unknown or ambiguous syntax stays incomplete, so
`only_arguments` rules do not match).

### Evidence

User profile `~/.ylva/profiles/default/config.toml` (`[tools.bash]`):

- Grammar-backed allow-lists exist for `cat`, `find`, `git`
  (`check-ignore`, `diff`, `status`, `log`, `show`), `grep`, `head`, `ls`, `rg`,
  `sort`, `tail`, `wc`.
- Deliberate grammarless broad rules: `nl`, `cut`, `tr`, `echo`, `printf`, `test`.
- Not covered at all (always prompt): `sed`, other `git` subcommands, `grep`
  with options after the pattern, and everything else below.

Replay corpus `tests/sandbox_evaluation/data/copilot-bash-commands-1792598338.json`
(15053 recorded scripts, git-ignored, local only; segments counted by splitting
on `| || && ; newline`). Prompting commands by frequency, read-only or
potentially read-only candidates only:

| Command | Count | Notes |
|---|---|---|
| `sed -n 'N,Mp' file` | ~3100 | Highest-value gap. Dangerous forms (`-i`, `w`, `e`, `r`, `s///e`, `-f`) need script validation. |
| `git stash list` | 25 | Bare `git stash` is `push`; requires nested subcommand check. |
| `git ls-files` | 21 | Read-only. |
| `git grep` | 13 | `-O`/`--open-files-in-pager` runs a program. |
| `git blame` | 6 | Read-only. |
| `git rev-parse` | 4 | Read-only. |
| `grep PATTERN -A40 file` | n/a (ideas doc) | Option after operand; blocked by BSD-conservative placement rule. |
| `nl`, `cut`, `tr`, `echo`, `printf`, `test` | 150, 106, 14, 1986, 176, 203 | Already broadly approved by the user. No dangerous options, so they stay broad; only `printf` (`-v`) gets a grammar (Phase 4). |

Deliberately **not** candidates (documented so nobody re-proposes them):

- `cargo build/test/check/clippy`, `make`, `xargs`, `env`, `python3`, `rumdl fmt`:
  they run arbitrary code or write files.
- `awk`: programs can call `system()`, `getline`, and redirect output; needs a
  dedicated program validator, not an option table. Remains deferred (ideas doc).
- `cd` and other shell-state commands: explicitly outside the roster.
- `git branch`, `git remote`, `git config`, `git reflog`: operands change their
  meaning from list to mutate; deferred.
- Bash operand path scoping and OS isolation: out of scope per
  `docs/ideas/bash-approval.md`.

### Acceptance criteria

- `sed -n '20,150p' file`, `sed -n -e '1,5p' file`, `sed -n '$p' file` are
  `complete` under a `only_arguments = ["-n", "-e"]` style rule; every form that
  can write, execute, or read a script file is incomplete.
- `git ls-files`, `git grep`, `git blame`, `git rev-parse`, `git stash list`
  (and `git stash show`) have scoped option tables; bare `git stash`,
  `git stash push|pop|drop|...` stay incomplete.
- `grep -n "text" -A40 path` is recognized if (and only if) Phase 3's
  verification shows both GNU and BSD/macOS grep permute; otherwise it is
  documented as intentionally unsupported.
- Unknown options, missing values, ambiguous bundles remain incomplete; the
  existing generated unknown-arity and bundle tests include the new grammars.
- Replay report on the local corpus shows fewer `user_approval_required` for the
  targeted commands and no newly auto-approved dangerous command (compare with
  the existing `*.report.json` baseline).
- `make format lint`, `make test-coverage` pass; docs updated (final step).

### Constraints

- Follow `AGENTS.md`: full descriptive names, license header on every new file,
  `foo.rs` module layout (no `mod.rs`), docs in the same change set.
- Grammars are static: no executable lookup, no environment inspection.
- No ADR covers this area (`docs/architecture-design-records/` holds only 0001-0003:
  git hooks, release-plz, workspace trust). No new ADR needed unless the sed
  script validator is widened beyond the stated subset.
- Recognition is not approval; the user's rules stay the only approval source.

## Implementation Plan

An implementing agent must flip `[x]` to `[x]` immediately after completing each
step and rewrite this file in place, so progress is resumable across sessions.
Write tests first (TDD) in `src/sandbox/command_arguments/tests.rs`; run
`cargo test --lib command_arguments` after each step.

1. [x] **Phase 0 – single grammar registry (SSOT).** In
   `src/sandbox/command_arguments.rs`, replace the three parallel lists
   (`supports_command` matches!, the `parse` match, `utility_options::options`)
   with one lookup (e.g. `fn grammar_for(name) -> Option<Grammar>`), so adding a
   command touches one place. Likewise derive git's subcommand list in
   `git.rs` (`parse` match and `scoped_options` match) from one table. Behavior
   unchanged; existing tests must pass untouched.
2. [x] **Phase 1 – `sed`.** Add `src/sandbox/command_arguments/sed.rs`.
   1. [x] Failing tests first: complete cases (`sed -n 20,150p f`, `sed -n -e 1,5p f`,
      `sed -ne '$p' f`, `sed --quiet --expression=2~3p f`, `sed -E -n /foo/p f`,
      `sed -n -s 1p a b`), and incomplete cases (`-i`, `-i.bak`, `--in-place`,
      `-f file`, `--file`, `-z`-less unknown options, scripts containing `w`, `W`,
      `e`, `r`, `R`, `s` (phase-1 subset), `{`, `y`, `a`/`i`/`c`, `;w`, newline-separated
      `w`, address-less garbage, missing script, `sed` with only options).
   2. [x] Option table `SED`: `flag -n/--quiet/--silent`, `-E/-r/--regexp-extended`,
      `-s/--separate`, `-u/--unbuffered`, `--posix`, `--help`, `--version`,
      `required -e/--expression`. Omit `-i`, `-f`, `-l`, `--sandbox`, `--debug`,
      `--follow-symlinks`, `-z` (conflicting or write/read-file semantics; unknown →
      incomplete).
   3. [x] Script source: with at least one `-e`, each `-e` value (attached, separate,
      or `--expression=`) is a script and all operands are files; otherwise the first
      operand is the script. Options only before the first operand (BSD-safe, as in
      `parse_options`).
   4. [x] Script validator (pure function, unit-tested separately): commands split
      by `;` or newline; each is `[addr[,addr]][!]cmd` with `cmd ∈ {p, d, q, =}`;
      `addr` ∈ decimal number, `$`, `first~step`, or `/regex/` where the regex
      contains none of `/`, `\`, `[`, newline (reject on doubt; no custom `\c…c`
      delimiters). Anything else, including empty scripts and `s`, makes the
      analysis incomplete. Surrounding blanks allowed.
   5. [x] Wire into the registry from step 1; add `"sed"` to the generated
      unknown-arity and bundle tests at the end of `tests.rs`.
3. [x] **Phase 2 – more `git` subcommands** in `src/sandbox/command_arguments/git.rs`.
   Each gets a scoped option table (documented URL + review date in a comment,
   like existing tables); global options keep their separate scope.
   1. [x] `ls-files`: `-c/--cached`, `-o/--others`, `-i/--ignored`, `-m/--modified`,
      `-d/--deleted`, `-s/--stage`, `-u/--unmerged`, `-z`, `-t`, `-v`,
      `--exclude-standard`, `--full-name`, `--error-unmatch`, `--deduplicate`,
      `--eol`, `--abbrev`(optional), `--` paths. Exclude `-x/--exclude`,
      `-X/--exclude-from`, `--exclude-per-directory` (read files; unrecognized,
      hence incomplete), `--recurse-submodules`, `--sparse`.
   2. [x] `blame`: `-L` (required), `-l`, `-t`, `-s`, `-e/--show-email`, `-w`, `-p/--porcelain`,
      `--line-porcelain`, `--incremental`, `-f/--show-name`, `-n/--show-number`, `-M`/`-C`
      only via optional-attached spelling rules already used for diff. Exclude
      `--contents` (reads arbitrary file), `-S` (reads revs file), `--ignore-revs-file`.
   3. [x] `rev-parse`: `--git-dir`, `--show-toplevel`, `--show-cdup`, `--show-prefix`,
      `--is-inside-work-tree`, `--is-bare-repository`, `--abbrev-ref`, `--short`
      (optional), `--verify`, `--quiet`, `--symbolic`, `--symbolic-full-name`.
      Exclude `--parseopt`, `--sq-quote`, `--local-env-vars`-style stdin modes.
   4. [x] `grep`: shares the existing grep option vocabulary where it matches
      (`-n`, `-i`, `-v`, `-w`, `-F`, `-E`, `-e`, `-l`, `-L`, `-c`, `-A/-B/-C`, `--no-index`,
      `--untracked`, `--cached`, `-I`, `-a`, `-h`, `-H`, `--max-depth`, `-q`, `-o`,
      `--heading`, `--break`, `-p`, `--` paths). Exclude `-O/--open-files-in-pager`
      (runs a program) and `-f` (not needed; keep unknown). Require a pattern as for
      `grep`/`rg` standalone-mode handling.
   5. [x] `stash`: after the `stash` identity, the next token must be `list` or
      `show` (identified as its own identity so `only_arguments = ["stash", "list"]`
      works); any other or missing nested token is incomplete. `list`/`show`
      accept the `HISTORY`/`DIFF_OUTPUT` tables as for `log`/`show` (`list` is
      log-options).
   6. [x] Tests for each: aliases, `--` handling, values not impersonating
      subcommands, dangerous forms recognized-but-not-approved (`grep -O`,
      `stash push`, `stash` bare, `ls-files -x pattern`).
4. [x] **Phase 3 – `grep` option permutation.** Decided: allow it for `grep` only.
   1. [x] macOS/BSD grep permutes (verified: `grep needle -A1 file` works on
      Darwin). Confirm GNU grep permutation from the GNU grep manual (record URL and
      date in the code comment) and document the `POSIXLY_CORRECT` caveat
      (disables permutation in GNU) as a residual risk in `docs/configuration.md`.
   2. [x] Add a per-grammar `permutes_options` property (true for `grep`; `rg` already
      interspersed) and make `parse_options` honor it instead of the
      `saw_operand && name != "rg"` special case. Test `grep needle -A8 file` and
      `grep -n "some text" -A40 path` complete with `-A` identified; all other
      utilities stay first-operand-terminated.
   3. [x] If the GNU docs contradict permutation, revert to the current behavior, pin
      `grep needle -A8 file` as incomplete in a test and document the finding.
5. [x] **Phase 4 – `printf` grammar.** Selection rule: a grammar is added only for
   tools with dangerous options. Of the broad rules in the user config only `printf`
   qualifies (bash builtin `printf -v var` assigns shell state). `nl`, `cut`, `tr`,
   `echo` and `test` have no dangerous options and stay broad.
   1. [x] Failing tests first: `printf '%s\n' x` and `printf -- '%s' x` complete;
      `printf -v var '%s' x` and any other `-x` first token incomplete.
   2. [x] Add the grammar (no options; only `--` accepted) via the registry from step 1,
      and add `printf` to the generated unknown-arity tests.
6. [x] **Phase 5 – validation.**
   1. [x] Add expectation entries (the replay runner supports `expectations`; see
      `tests/sandbox_evaluation/README.md`) for the new forms and the dangerous
      near-misses: `sed -i`, `sed -n 'w out'`, `sed -n '1e id'`, `git grep -O`,
      `git stash`, `git stash push`.
   2. [x] Extend `tests/sandbox_evaluation/sample.config.toml` with the new narrow rules.
   3. [x] Run `make data/<corpus>.report.json` with a rule set containing the new rules;
      compare to the existing `*.report.json`: the newly approved commands must all
      be in the targeted set (diff `sandbox_verdict` per id and review every flip).
7. [x] **Docs (same change set).**
   1. [x] `docs/configuration.md` → *Bash command rules*: update the roster sentence
      (currently "`cat`, `ls`, … and `git` with `check-ignore`, `status`, `log`, `diff`, or
      `show`"), remove "`sed` and `awk` have no conditional grammar" → "`awk` has no
      conditional grammar", document the `sed` subset and script limits, the new
      git subcommand scopes and exclusions, the grep placement outcome, and add an
      example allow-list rule for `sed`.
   2. [x] `docs/implementation-notes.md` → *Argument-conditioned command rules*: registry,
      `sed` script validator, nested `git stash` subcommands, permutation property.
   3. [x] `docs/features.md` (line ~117 mentions grammars): adjust if the summary lists
      the roster.
   4. [x] `docs/ideas/bash-approval.md` → *Grammar coverage*: remove the delivered
      `sed -n …` and `grep -n … -A40` examples and the "`sed` … remain potential additions"
      statement; keep `awk`.
   5. [x] `tests/sandbox_evaluation/README.md` if the expectation workflow text changes.
   6. [x] `rumdl check` on touched markdown (`rumdl fmt` to fix).
8. [x] **Verification.** `make format lint`, then `make test-coverage` (single
   mandatory test gate). No dependency changes expected, so `make audit` not needed.

Implementation notes: the sed script validator became a fixed safety floor and
the rule field `allowed_scripts` (required on `sed` rules, regexes matched per
script command) decides what a rule approves. `cd` was added to the roster during execution (one literal
operand; bare `cd` and `cd -` incomplete) because every bash call is a fresh
shell, so `cd dir; cmd` equals `cmd dir/...`. `printf` rejects only a first-argument
option (later `-...` tokens are format arguments). Replay: no previously approved
command lost approval; expectations in
`tests/sandbox_evaluation/grammar-expectations.json`.

## Proposed Improvements

- Use the replay report's "approved only because of grammar X" data to rank the
  next candidates, as the ideas doc suggests (needs the verdict trail in
  `docs/ideas/approval-verdict-trail.md`).

## Implementation Hints

Decisions settled in grilling round 1:

- `sed`: simple regex addresses are in scope (no `/`, `\`, `[`, newline inside the
  regex); `s///` is deferred; the fuller option set (`-n -e -E -r -s -u --posix
  --quiet --silent --expression`) is recognized, since recognition is not approval.
- Git options that read files (`ls-files -x/-X`, `blame --contents/-S/
  --ignore-revs-file`) stay unrecognized/incomplete; `git stash` supports only
  nested `list` and `show`.
- `grep` permutation follows verification (macOS verified; GNU via docs).
- Selection rule: grammars only for tools with dangerous options, so Phase 4 is
  reduced to `printf`; `nl`, `cut`, `tr`, `echo`, `test` stay broad. No additional
  corpus candidates beyond the planned git
  subcommands (`diff`, `du`, `which`, `od`, `uniq`, `ps`, `sleep` are out).
- Grammar entry points: `src/sandbox/command_arguments.rs`
  (`supports_command`, `parse`, `parse_options`, `place_option`, `consume_option`,
  `Arity::{Flag,Required,RequiredEquals,OptionalAttached}`, helpers `flag`/`required`/
  `equals`/`optional`), `command_arguments/utility_options.rs` (tables for
  cat/ls/head/tail/wc/grep/rg/sort; `options(name)` lookup), `command_arguments/find.rs`,
  `command_arguments/git.rs` (`GLOBAL`, per-subcommand tables, `parse`,
  `scoped_options`, `valid_global_spelling`), tests in
  `command_arguments/tests.rs` (helper `command(&[&str])`, `canonical`; generated
  unknown-arity/bundle tests at the end).
- `src/sandbox/rules.rs:~570` rejects conditional rules for commands where
  `command_arguments::supports_command(name)` is false, so a new grammar must be
  registered there (the registry from step 1 makes that automatic). Selector
  matching and verdict ordering live in `src/sandbox/command.rs`; do not change them.
- `ArgumentAnalysis` fields: `identities` (alias lists, matched by globs),
  `complete`, private `operands`, `option_values` (token positions). An unknown
  or invalid sed script should set `complete = false` and add no identity; a valid
  script is an operand/option value, so it never impersonates an option.
- `consume_option` for `-e` with an attached value (`-es/x/`) records the same token
  position in `option_values`; strip the two-character prefix before validating.
  Long form: `--expression=SCRIPT` (split on first `=`).
- `ArgvToken.text` already joins partly quoted pieces; variable/command
  expansion inside a token makes the command un-analysable before the grammar
  runs, so the sed validator sees only literal text.
- `head`/`tail`/`git log` numeric `-N` handling is the pattern for special
  spellings; `-N` for sed is not applicable.
- Git globals (`-C`, `-c`, `--no-pager`, …) are already parsed before the
  subcommand, so `git --no-pager blame …` and `git -C dir ls-files` need no extra work.
- Existing grep placement rule: `parse_options` sets `complete=false` when an
  option follows an operand unless the command is `rg` (comment cites BSD getopt
  stopping at the first operand; this is unverified for macOS `grep`).
- Replay: `tests/sandbox_evaluation/` (README, `sample.config.toml`,
  `bash_policy_replay.rs`, Makefile target `make data/sample.report.json`).
  `data/` is git-ignored; the corpus and baseline reports exist only on the
  author's machine. Corpus counts above are approximate (naive segment split).
- User config `~/.ylva/profiles/default/config.toml` is outside the repo; the
  plan never edits it. After release the user can add, e.g.
  `{ command = ["sed", "**"], verdict = "auto_approved", only_arguments = ["-n", "-e"] }`.
- Verification commands are mandated by `docs/development-workflow.md`:
  `make format lint` and `make test-coverage`.
