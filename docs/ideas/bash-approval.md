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
   (per-command validators with allowed flag sets). Config sketch:
   `{ command = ["find", "**"], deny_args = ["-exec", "-execdir", "-ok",
   "-delete", "-fprint*", "-fls"], verdict = "auto_approved" }` or inverted
   `allow_flags = [...]`. Prefer allow-lists of flags over deny-lists; unknown
   flag ⇒ ask. Reject combined short flags that cannot be parsed (`-i.bak`,
   `-ni`) rather than guessing.
2. **Remove inherently risky commands from the default list.** `echo`/`printf`
   are safe, but `sed`, `awk`, `find`, `xargs`, `env`, `sort -o`, `tee`,
   `git -c`, `tar`, interpreters (`python`, `node`) can never be classed
   readonly by name alone. `find` needs layer 1; `sed`/`awk` only in a
   whitelisted form or not at all. Suggest dedicated tools (`read`, `glob`,
   `grep`) so the model needs bash less.
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

### Recommendation

1. Short term: add `deny_args`/`allow_flags` rule fields (layer 1), make
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
