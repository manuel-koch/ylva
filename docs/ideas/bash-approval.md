# Bash approval

How can we find out if a given `bash` command is `readonly` and
could be auto-approved ?

There are commands that are in basically `readonly`, but have certain
arguments that can make them `write` or `edit` something:

- `find . -name "*.md" -exec rm {} +`
- `sed -i '' 's/old_string/new_string/g' file.txt`

But even if we find a list of assumed `readonly` commands couldn't anybody
( even the LLM ), just create an executable that is placed in the
PATH and performs "evil" side-effects despite its well-known name,
like an evil-twin of `cat` ?

Such a positive list of `readonly` commands is at best a convinience
for auto-approval and in no way a security/sandbox feature, isn't it ?

See [bash tool config](../../.ylva/profiles/other/config.toml)
for some command rules.

## Research findings

### Starting point in ylva

`src/sandbox/bash_command_extractor.rs` already parses commands with
`tree-sitter-bash` and only analyses `list`/`pipeline`/`command` nodes.
Redirections, substitutions, subshells, control flow, heredocs and
backgrounding are `Unanalysable` (never auto-approved). Rules are argv globs
(`["cat", "**"]`), i.e. they match the command *name* and accept any arguments.
This closes the shell-syntax holes (`cat x > y`, `$(rm …)`) but not the
flag holes of a command itself.

Tilde expansion is not detected: `~`, `~user`, `~+` and `~-` are kept as
literal word text, while the equivalent `$HOME/x` is already
`Unanalysable`. Harmless today (no path checks for `bash`; `cat ~/x` is
no worse than `cat /Users/me/x`), but it matters for path scoping
(layer 5), see there.

### Answer to the open question

Detecting "readonly" **reliably** is impossible in general, so yes: an
allow-list is a convenience feature, not a security boundary. Reasons:

- Evil twin / `PATH` hijack: the name `cat` says nothing about the binary
  that runs. A model with `write` access can create `./cat` or edit
  `~/.zshrc`, and the next "readonly" call executes it.
- Semantics depend on arguments, environment and files, not only the name
  (`sed -i`, `sort -o`, `find -exec/-delete/-fprint`, `awk 'system(..)'`,
  `git -c core.pager=…`/`--output=`, `rg --pre <cmd>`, `tar`, `xargs`, `env`,
  `less`/`man` pagers, `sh -c`).
- Leading `VAR=value` assignments (`PAGER=… git log`, `LD_PRELOAD=… ls`,
  `GIT_EXTERNAL_DIFF=…`) change behavior. The extractor sees them; rules
  should treat them as unanalysable unless the variable is allow-listed.
- "Readonly" is not "harmless": `cat ~/.ssh/id_rsa`, `env`, `printenv` or
  `curl` read secrets that then flow into the model context or the network.
  Read-only-ness needs a *path/scope* check, not just a command check.
- Aliases/functions are not an issue for non-interactive `bash -c`, but
  shell options and `BASH_ENV` can be.

### Concepts (prior art and options)

Layers, cheapest first. Ideas are additive.

1. **Per-command flag/subcommand allow-list (positive, not `**`).**
   Used by OpenAI Codex CLI (`is_known_safe_command`: tree-sitter-bash, fixed
   command set, rejects `find -exec/-delete/-fprint*`, `rg --pre/--hostname-bin`,
   `base64 -o`, `git` only `status|log|diff|show|branch` without
   output/exec flags, `sed` only the `-n Np` form) and by Claude Code
   (per-command validators with allowed flag sets). The verdict already says
   allow or deny, so the argument condition fields only describe *when the
   rule applies* (flags and subcommands alike are "arguments"):
   - `with_arguments`: rule applies if **any** listed argument is present
     (deny-list style, glob patterns allowed).
   - `only_arguments`: rule applies only if **every** option/subcommand
     argument is in the list (allow-list style); an unlisted argument means
     the rule does not match and other rules decide (default floor).

   ```toml
   { command = ["find"], with_arguments = ["-exec", "-execdir", "-ok",
     "-delete", "-fprint*", "-fls"], verdict = "user_approval_required" }
   { command = ["git"], only_arguments = ["status", "log", "diff", "show"],
     verdict = "auto_approved" }
   ```

   Prefer `only_arguments` over `with_arguments`; an unknown flag means the rule does not match.

   *Argument conditions need a per-command grammar.* There is no generic
   way to tell options, subcommands, option values and operands apart:
   `find` puts paths first and uses predicates (`-exec`) instead of options;
   `git` has global options before the subcommand (`git -C dir -c k=v log`);
   `tar xf a.tar` / `ps aux` have dash-less bundles; the first operand of
   `sed`/`awk` is a program, not a path; GNU tools permute options and
   operands (`ls file -l`); `-C dir` / `--pre cmd` take values. Therefore:
   - Ylva ships a small **built-in grammar table** (code, not config) for
     the few commands that may be auto-approved with arguments. Per command:
     known options with arity (flag or takes a value), subcommands and
     global options, operand kinds (path, pattern, program).
   - Grammars are **handcrafted per tool** but kept as small static data
     tables walked by one shared generic parser; only odd tools (`find`
     predicates, `tar xf` bundles, `sed` programs) need special hooks.
     Scope stays small: only commands worth auto-approving with arguments
     (e.g. `git`, `find`, `ls`, `cat`, `grep`/`rg`, `head`/`tail`, `sort`,
     `wc`). An option missing from the table makes the command unplaceable (see
     below), so an incomplete table is safe, only less convenient. Not generated from `--help`/man
     pages/completions (untrusted source for a security decision);
     user-defined grammars in config are a possible later extension.
     Validate against the GTFOBins corpus (see References).
   - `with_arguments` / `only_arguments` are evaluated on the **parsed
     result** (options and subcommand), never on raw tokens. Operands are
     not matched against the lists; paths go to path scoping (layer 5).
     A file named like a listed argument (`status`, `./-exec`) is therefore
     harmless.
   - **Selecting the grammar:** implicit by the bare command name
     (`command[0]`, layer 3 already refuses paths and wrappers). No `grammar`
     field (YAGNI); aliases/wrappers would be an explicit built-in entry.
   - A rule with `with_arguments`/`only_arguments` for a command without a
     built-in grammar is a **configuration error** (rejected at load, not
     silently ignored). Such commands only support rules without argument
     conditions (plain pattern matching as today).
   - **Grammar is engaged only by an argument condition.** A rule without
     `with_arguments`/`only_arguments` is plain pattern matching, even for
     commands that have a built-in grammar: `{ command = ["git", "**"],
     verdict = "auto_approved" }` means "any `git` invocation, unparsed" and
     is an explicit, user-owned choice. Shipped default rules must never use
     a bare trailing `**` for commands with a grammar; they use
     `only_arguments`. Rule order stays last-match-wins, so a specific
     `with_arguments ... verdict = "user_approval_required"` rule placed
     after the wildcard vetoes commands where the listed argument is recognised (see below).
   - **The grammar never produces a verdict**, it only parses. Unknown
     option, ambiguous token, unparsable bundle (`-ni`, `-i.bak`) or
     anything it cannot place yields *unplaceable* for that token. The
     conditions treat it as follows:
     - `with_arguments` is a presence check and needs only a partial scan:
       it matches when a listed argument is *properly placed* by the
       grammar (as option or subcommand), even if other tokens are
       unplaceable. Independent of the rule's verdict; unplaceable or
       ambiguous tokens are not evidence and neither match nor block (an
       option value such as `find -name -exec` is not an option). Whether a
       verdict is sensible for "argument present" is the rule author's
       responsibility; shipped approving rules use `only_arguments`.
     - `only_arguments` must know every argument: any unplaceable token
       makes it not match.
     - A rule that does not match makes no decision; other rules or the
       default floor (user approval) decide. Thus a `with_arguments` veto
       after a bare wildcard catches `find . -weird -exec`, but can be
       evaded when the listed argument is not recognisable in any token
       (e.g. hidden in an unparsable bundle). That wildcard is the user's
       explicit unparsed opt-out; the safe form is `only_arguments` on the
       approving rule.

2. **Remove inherently risky commands from the default list.** `echo`/`printf`
   are safe, but `sed`, `awk`, `find`, `xargs`, `env`, `sort -o`, `tee`,
   `git -c`, `tar`, interpreters (`python`, `node`) can never be classed
   readonly by name alone. `find` needs layer 1; `sed`/`awk` only in a
   whitelisted form or not at all. Suggest dedicated tools (`read`, `glob`,
   `grep`) so the model needs bash less.

   *Shell-state commands in a list.* Commands in a list run in one shell,
   so state changed by an earlier command affects later ones (e.g.
   `export PATH=/tmp/evil; cat x` runs an evil-twin `cat`). Forbidding
   state changes altogether is impractical (models constantly `cd` to the
   workspace or a temp dir) and `cd` is harmless as long as nothing
   auto-approved depends on the working directory (unquoted globs require
   approval, `$…` is `Unanalysable`, no path checks). The guard is instead:
   - No shipped rule auto-approves a state-changing builtin: `export`,
     `declare`/`typeset`/`readonly`/`local`, `set`, `shopt`, `source`/`.`,
     `alias`, `hash`, `enable`, `eval`, `exec`, `trap`, `builtin`,
     `command`. `export`/`declare`-style commands already parse as
     declaration commands and are `Unanalysable`; the others are ordinary
     commands and rely on not being allow-listed.
   - A list is auto-approved only if **every** command in it is
     auto-approved by a rule.

   *Known bug (verified 2026-10-21):* the second point does not hold yet.
   `SandboxContext::check_command` combines per-command verdicts by
   worst-case ranking, but a command matching no rule contributes `None`,
   which loses to `AutoApproved`. With `{ command = ["cat", "**"],
   verdict = "auto_approved" }`, `rm -rf foo; cat x` is auto-approved. An
   unmatched command inside a list must count as `UserApprovalRequired`
   (only a fully unmatched script may abstain).
3. **Treat env assignments, absolute/relative command paths and wrappers
   as unanalysable.** Match only bare names; refuse `./cat`, `/tmp/cat`,
   `env cat`, `command cat`, `nice`, `time`, `sudo`, `xargs`.
4. **Resolve the executable (anti evil-twin, best effort).** Resolve the name
   against a *fixed trusted* `PATH` (`/usr/bin:/bin:/usr/local/bin`,
   Homebrew prefix) and run the command with that sanitized `PATH`; refuse
   when the resolved file is inside the workspace or writable by the user.
   Optionally pin hashes/ownership. Mitigates, does not eliminate (the
   model can still rewrite `~/.zshrc` via the `write` tool; that tool call
   has its own approval).
5. **Path scoping for read commands.** Resolve file arguments (incl. globs,
   `~`, `..`, symlinks) and auto-approve only inside the workspace/allow-list;
   deny dotfiles with secrets (`.env`, `.ssh`, credentials). Reuses the
   existing `read` tool path rules (`src/sandbox/path`) = SSOT for what may
   be read.

   *Tilde is an expansion, not literal text.* Bash expands an unquoted
   leading `~` at execution time of each command, like `$HOME`:
   - Taken literally, `~/.ssh/id_rsa` looks like a workspace-relative path
     (a directory named `~`) and would be misjudged as inside the
     workspace.
   - The result depends on state an earlier command in the same list can
     change: `~` on `HOME` (`export HOME=/etc; cat ~/passwd`), `~+`/`~-`
     on `PWD`/`OLDPWD` (`cd`), `~user` on the user database.

   Before path scoping relies on argv text, either treat an unquoted
   leading `~` (`~`, `~/…`, `~user`, `~+`, `~-`) as `Unanalysable`
   (consistent with `$HOME`), or expand it in ylva with a fixed `HOME` and
   only when no earlier command in the list can change `HOME`/`PWD`.
   Quoted `"~"` is literal and needs no handling.
6. **Real sandbox for execution (the only reliable "readonly").** Run the
   command under an OS sandbox with a read-only filesystem and no network:
   macOS Seatbelt (`sandbox-exec`, deprecated but used by Codex and Claude
   Code), Linux Landlock/seccomp or `bubblewrap`. Then "readonly" is enforced
   by the kernel, regardless of command name, evil twins or flags; an
   attempted write simply fails. Auto-approve = "runs in read-only sandbox".
   Costs: platform-specific code, some tools break (need temp dirs,
   caches), Seatbelt is formally deprecated, no Windows support. Keep
   the allow-list as fast path and the sandbox as the actual guarantee.
   Needs an ADR (this is a new security boundary).
7. **Learned/approved-prefix rules ("always allow `git log`").** Used by
   OpenCode and Claude Code: when the user approves, offer to persist a rule
   for the command *prefix* (`git log *`), based on a per-command "arity"
   table (how many tokens identify a subcommand). Reduces prompts without
   shipping a risky default list; rules stay user-owned. Pair with
   `approval-verdict-trail.md`.
8. **Additional heuristic scanners** such as `tirith`
   ([idea](shell-hardening-with-tirith.md)) as a veto layer (can only
   downgrade `auto_approved` to ask). Subprocess only (AGPL).
9. **LLM judge** for ambiguous commands: unreliable, prompt-injectable,
   costs latency; only acceptable as a veto, never as the approver.
10. **Fail closed + explain.** Any unparseable, unknown or partially
    analysable construct ⇒ ask the user; show *why* in the approval prompt
    (e.g. "`find` with `-exec`"). Fuzz/property-test the extractor and the
    flag tables against a corpus of known bypasses (GTFOBins list is a good
    source for "readonly" commands that can write/execute).

### Building the built-in grammar table

*Which commands to include.* Start from evidence, not from a wish list:

1. **Usage-driven:** collect the commands the model actually runs and the
   user approves repeatedly (approval trail, see `approval-verdict-trail.md`;
   layer 7 prompts). Rank by frequency; include only the top few.
2. **Read-only value:** include a command only if it has a useful read-only
   subset (`git status|log|diff|show`, `ls`, `cat`, `head`, `tail`, `wc`,
   `grep`/`rg`, `sort`, `find`). Skip commands whose useful forms write or
   execute (`tee`, `tar`, `xargs`, `env`, interpreters); they stay
   approval-required.
3. **Risk screen:** check each candidate against GTFOBins/LOOBins
   capability tags (*Shell*, *Command*, *File write*, *File read*,
   *Upload*). Options carrying those capabilities are simply not whitelisted;
   commands where nearly every option is tagged are dropped.
4. **Maintainability:** prefer tools with a small, stable option set.
   Large, fast-changing CLIs (`git`) are supported only for a pinned
   subcommand subset.

*How to set up one grammar.* Per command, in this order:

1. List **all** options from the man page/`--help` of the supported
   platforms (macOS BSD vs. GNU differ) with arity: flag, value, optional
   value; long/short aliases; `--opt=value` form; whether short flags
   bundle.
2. Classify each option **safe** (whitelisted), **unsafe** (known capability,
   listed explicitly with a comment why, so reviews see it was considered) or
   **unknown** (default; makes the command unplaceable).
3. Declare structure: subcommands and global options, where operands may
   appear (before/after/between options, `--` terminator), operand kinds
   (path, pattern, program, revision).
4. Add special hooks only where data cannot express it (`find` predicate
   expression, `tar` dash-less bundle, `sed -n Np`).
5. Add tests: golden parse cases per command, a table-driven bypass corpus
   from GTFOBins/known CVEs (every accepted combination must not match a
   dangerous technique), and a property test that no input panics and unknown
   tokens always end as unplaceable.
6. Record the source (man page version, OS) and review date in the table, so
   staleness is visible; re-review on new OS/tool releases.

*Rollout.* Add grammars incrementally, one command per change, each with its
tests and doc update. A command without a grammar keeps today's behavior
(exact rules or approval), so partial coverage is never a regression.

### Recommendation

1. Short term: add `with_arguments`/`only_arguments` rule fields (layer 1), make
   `find`, `sed`, `awk`, `sort`, `git` explicit flag-aware entries, reject
   env-assignment prefixes and path-qualified command names (layer 3).
2. Add path scoping reuse for read-style commands (layer 5).
3. Add prefix-learning on approval (layer 7).
4. Mid term: evaluate an optional OS sandbox (layer 6) as the only true
   guarantee; record in an ADR. Until then, document clearly that bash
   auto-approval is a convenience feature, not a security boundary.

Open points to verify before implementing: current Seatbelt/Landlock
availability and Rust crates, Codex/Claude Code behavior may have changed
since this was written (findings are from prior knowledge, not re-verified
against current sources).

### Relation to redact-sensitive-info

[redact-sensitive-info](redact-sensitive-info.md) partly closes the
"readonly is not harmless" gap: values from ylva's profile `.env` are
scrubbed from tool results, so `cat`/`env`/`printenv` cannot leak them into
the LLM context. It does not cover:

- Secrets outside `.env` (`~/.ssh`, `~/.aws/credentials`, `~/.netrc`, other
  projects' `.env` files) — needs path scoping (layer 5).
- Transformed values (`base64`, `rev`, `fold`, `cut`, hex).
- Exfiltration that never appears in a tool result (`curl -d @file`, `nc`) —
  needs network rules / `tirith`.
- Execution-time side-effects: redaction runs after the command ran.

Treat it as a complementary defense-in-depth layer, not a replacement for
flag allow-lists, path scoping or an OS sandbox.

### References

- [GTFOBins](https://gtfobins.org): Unix binaries abusable for file
  read/write, shell, command execution; capability-tagged. Source/data:
  <https://github.com/GTFOBins/GTFOBins.github.io> (YAML per binary) — usable
  as a test corpus: no allow-listed command/flag combination may match a
  *Shell*, *Command* or *File write* technique.
- [LOOBins](https://www.loobins.io): macOS equivalent.
- [LOLBAS](https://lolbas-project.github.io): Windows equivalent.
- Prior art: OpenAI Codex CLI (`is_known_safe_command`), Claude Code
  permission/sandbox docs, OpenCode permission arity table.
