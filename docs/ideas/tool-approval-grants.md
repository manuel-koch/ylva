# Reusable tool-approval grants

> Implemented as a minimal grant dialog; see [features](../features.md#tool-approval-grants)
> and [configuration](../configuration.md#tool-approval-grants). Rule editing, the tool
> checklist and the per-row selector are not implemented.

Offer user-owned grants for similar tool calls when a tool requests approval.
This is a standalone feature: it does not depend on self-improvement, a
particular skill, or changes to `clarify`.

Grants reduce repeated prompts without replacing existing sandbox rules or
turning approval into an operating-system isolation boundary. Persistent grants
use a workspace-local per-profile file; the exact schema is still open.

## Approval flow

1. Keep the existing approve-once and deny actions, including their default
   behavior. Reusable grants require an explicit choice.
2. Offer a temporary grant or a persistent grant when the requested call has a
   supported matcher. Otherwise offer only the existing one-time decision. A
   Bash call with several commands gets one row per command (see *Compound
   calls* below).
3. Default to the requesting tool and its specific resource. A temporary grant
   defaults to the requesting agent; a persistent grant applies to all agent
   types and roles. The user can explicitly widen the resource, select
   additional compatible tools, or share a temporary grant with more agents.
   Let the user edit the proposed rule; reject an invalid rule or one that
   does not match the current request, with an explanation. Scope is chosen
   per grant; a Bash dialog chooses it per row (default temporary).
4. Preview the exact matcher, affected tools and agents, scope, and
   consequences before confirmation. For a persistent grant, also show the
   file to be written, the resolved base and scope for any path matcher, and
   any workspace marker that will be created. Warn that matching calls already
   waiting for approval may proceed after confirmation.
5. Apply a confirmed temporary grant immediately. Apply a persistent grant only
   after saving it successfully; if the save fails, leave the original approval
   pending and let the user choose another action.

A preview for a file mutation must say that the grant permits **any** later
content change by the selected tool at that path. A Bash wildcard must be shown
as a wider command scope, not as a promise that later commands are harmless.
Literal paths, hostnames, and Bash prefixes can contain sensitive information;
show what will be stored and warn before writing it.

## Approval decision and precedence

Validate the tool arguments and evaluate the existing tool policy before
considering grants:

1. A schema error or effective `auto_denied` verdict is denied, even if a grant
   matches. Keep the original denial reason.
2. An existing `auto_approved` verdict proceeds as before.
3. For an ordinary `user_approval_required` verdict, a matching temporary or
   persistent grant permits the call; otherwise use the existing approval
   prompt. Headless mode continues to deny calls that still require approval.
4. Apply the tool's other existing execution checks as well. A grant cannot
   register a disabled tool, change an agent's allowed tools, or authorize a
   private network address.

Configured rule lists keep their current semantics: an agent-specific list
replaces the profile/default list, and the last matching configured rule wins.
CLI auto-approval flags retain their existing final precedence and can already
override an earlier denial. A grant cannot override the **resulting effective**
denial; it does not restore a denial already overridden by existing policy.

Grants are separate from those rule lists, not inherited rules that an
agent-specific list replaces. If an agent's own effective rule requires
approval, a matching grant satisfies that ordinary prompt. If its effective
rule denies the call, the grant does not permit it.

`clarify` is always a question-and-answer interaction, not a permission that a
grant may bypass. A future repeated-call loop safeguard must likewise be able to
require a human response even when a permission grant matches. Distinguish such
mandatory interaction from ordinary approval-required policy rather than
assuming every request with the same verdict is grantable.

## Tool-specific matching

### Files, search, glob, and skills

- Store a path matcher as a `rule` glob, not a path plus an
  `include_descendants` flag. Match the entire resolved target path; the
  pattern describes future calls as well as currently existing files.
- Grant rules use the existing custom-glob matcher documented in
  [sandbox tool-approval rules](../configuration.md#sandbox-tool-approval-rules):
  component-aware `*`/`?`/`[...]`, whole-component `**`, and the same
  dot-component rules. The bare `**` catch-all is an invalid grant rule
  (variants such as `**/*` stay valid); blanket-everything approval stays with
  the `--auto-approve-*` flags. Automatically generated exact rules must
  escape literal glob metacharacters.
- `read`, `write`, and `edit` match the resolved file path, not offsets,
  contents, or patches. A grant for one tool does not authorize either of the
  other tools unless the user explicitly includes it.
- `search` matches its file or directory search root, independently of the
  expression, filters, and other arguments. A directory-root grant can search
  descendants. Current `search` checks its root, not every encountered file;
  a per-file deny does not by itself prevent a descendant from being searched.
- `glob` matches the traversal root derived by the existing tool, not the
  pattern text. Other patterns and filters with the same root can match the
  grant. As with `search`, checks apply to that root rather than every result.
- `load_skill` matches both the selected skill name and its resolved
  instruction-source path. The skill must still be in the agent's callable
  catalog. The grant neither freezes future skill contents nor authorizes
  reading sidecars or executing anything.
- Resolve each requested path using the existing tool policy's expansion and
  resolution. Saved rules are globs, not shell expressions: do not expand
  `~` or environment variables in them. Do not add a separate inode or symlink
  identity system.
- Generate workspace-root-relative rules for targets inside the workspace and
  absolute rules for targets outside it. Also accept an explicitly chosen
  absolute rule inside the workspace and preserve the form the user chose.
  Relative rules resolve against the workspace root, never the launch
  directory. A relative grant matches only while its resolved target remains
  inside the workspace; reject escaping paths.

For example, `./notes.md` grants the exact file; `./*.md` matches non-hidden
Markdown files directly under the workspace root; and `./**/notes.md` also
matches that name beneath non-hidden subdirectories. `./.env` explicitly
matches the hidden file. An absolute rule such as `/work/*.md` is valid too.
These examples assume the existing policy would otherwise request approval; an
effective denial still wins.

The relative-path boundary checks the matched target or traversal root, not
every descendant of a `search` or `glob` traversal. Existing configured sandbox
path rules are evaluated first and share the same matcher.

### Bash

Match a single simple command by its literal command and argument prefix, with
an optional trailing `*` standing for one or more additional arguments. The
bare command without the suffix is a distinct, narrower grant:

| Grant | Matches | Does not match |
| --- | --- | --- |
| `foo` | `foo` | `foo bar` |
| `foo bar` | `foo bar` | `foo bar -a` |
| `foo bar *` | `foo bar -a`, `foo bar -a -b` | `foo bar`, `foo baz` |
| `foo -a *` | `foo -a file`, `foo -a file x` | `foo -a`, `foo -b` |

For `date ; pwd ; make foo`, a grant `make foo` matches only the third simple
command. The others are judged separately, never by one grant for the line.

Match whole argument boundaries, not raw string prefixes: `foo bar *` must not
match `foo barcode`. The optional wildcard is only a trailing suffix, not an
arbitrary glob or regular expression. Store the rule as a string, parse it
without executing it, and preserve quoting so a quoted literal `*` argument is
distinguishable from the suffix wildcard.

This grant syntax differs from existing configured Bash command rules: a
configured trailing `*` means exactly one argument and `**` means one or more,
while a grant's trailing `*` means one or more arguments and a `**` token is an
invalid grant matcher. Make the grant's one-or-more meaning explicit in the
preview and documentation.

As with a configured trailing-`*` command rule, a grant does not satisfy an
`unquoted_glob` or `unquoted_brace_expansion` verdict: `git log *` does not
approve `git log *.md`, which stays approval-required. A quoted literal
argument is covered (`git log -- '*.md'`). Outside the trailing suffix, a grant
matcher cannot contain an unquoted glob or brace character at all.

A grant never covers a pipeline, redirect, or command list as a whole. `foo bar *`
must not approve `foo bar; rm ...`. Instead, split a call into its simple
commands with the existing Bash command extractor and judge each on its own.

#### Compound calls

Show one approval dialog per Bash call with one row per analysable simple
command that policy does not already cover (for example `date ; pwd ; make foo ;
make bar` yields four rows). Each row has an editable single-line matcher and a
three-way selector `off | temporary | persistent` (default `off`). Rows already
`auto_approved` or already granted are read-only or collapsed into "N already
allowed". The full command stays visible in the expanded Bash bubble; the dialog
has no separate "show full command" control.

- Every command must be covered by policy, an existing grant, or a selected
  row. One effective `auto_denied` command denies the whole call before any
  dialog; grants cannot override it.
- Constructs the extractor cannot analyse (subshells, heredocs, command or
  process substitutions, control flow, non-harmless redirects) get no row. When
  a call has such parts, show one footnote: "Other parts of this call can only
  be approved once." Selected rows still create grants for future calls; the
  unanalysable parts never become grantable and always need the one-time
  approval. If nothing is analysable, show no checklist and no footnote.
- Confirming means "run this call and save the selected grants". If every
  uncovered command has a selected row, the call runs without a separate
  approve-once. Otherwise it runs once for the remainder. Deny saves nothing. A
  failed persistent save leaves the approval pending.
- Each row has its own scope. A temporary grant defaults to the requesting
  agent instance; a persistent grant applies per profile and workspace.

#### Proposing a matcher

Propose the leading tokens verbatim, flags included (`git log --oneline`,
`make -j4 foo`), capped at four tokens. Stop at the first token that is
path-like, quoted, or contains a glob or expansion. Append the trailing `*`
only when tokens were dropped — by the cap or a stop token — so a short
all-literal command proposes an exact matcher (`git status`) that also covers
the row's own command. Ylva has no per-program option grammar, so `-n 5` is
not understood as a flag plus value; the user edits the proposal as needed.
Show the matcher on one line, truncated with `…`; reject a matcher that
contains a newline.

The program token is matched literally as written: `make foo` does not match
`/usr/bin/make foo` or `./make foo`, and no `PATH` resolution or path
normalization applies. A program given as a path is grantable only as that
exact literal path. Environment is not part of the match, nor is the working
directory; `cd`, `pushd`, `export`, and `source` are non-grantable shell-state
commands, and the preview warns that the working directory is not matched.

#### Non-grantable rows

Keep a small static table in code (not configuration) of rows that never get a
grant, even if a user types one:

- Interpreters with inline-code flags: `python`/`python3 -c`, `node -e|-p`,
  `perl`/`ruby -e`, `php -r`, `sh`/`bash`/`zsh -c`. Do not parse script
  content. Interpreter-plus-file rows (`python script.py *`) are grantable but
  marked wider, because the file's content can change later.
- Wrappers that run another command: `env`, `xargs`, `sudo`, `nohup`, `time`,
  `timeout`, `find -exec`, `watch`. Do not unwrap them.
- Destructive programs (`rm`, `dd`, `mkfs`, `chmod`, `chown`, `mv`) with a
  trailing `*`; exact-argument grants stay allowed.
- Rows with environment-assignment prefixes (`FOO=1 make foo`) or `$VAR`/`~` in
  the matched tokens.

#### Editing a matcher

Accept an edited matcher only if it parses as a literal program, literal
arguments, and an optional trailing `*` (no other glob, regex, or expansion);
it matches the row's actual command; and its program token is unchanged (a bare
`*` or wildcard program is rejected). Re-apply the non-grantable table to the
edited string, so `python script.py` → `python *` is rejected. An edit may
narrow or widen the grant within the same program (`make foo` → `make *` is
legitimate); the preview marks a wider matcher as a wider scope. A suffix edit
must still match the row: from `make foo`, `make foo *` is rejected because it
requires one or more further arguments; widen as `make *` instead. An edit
changes only the stored grant, never the call that runs now.

Unsupported or ambiguous syntax outside these rules falls back to one-time
approval rather than guessing. An effective Bash denial still wins.

This matcher is deliberately simpler and potentially broader than the existing
argument-aware Bash rules. Even one matched program can invoke other programs,
read or write files, or use the network; operands, environment, executable
resolution, and configuration are not isolated.

### Web tools

- A `web_search` grant allows any search terms against the configured search
  backend. It remains applicable if the profile later selects another backend,
  subject to that request's existing effective policy. It does not grant
  `web_fetch` or Bash access.
- A `web_fetch` grant matches the **initial** URL's scheme, host, and effective
  port, without storing or matching its path or query. For example, a grant for
  `https://docs.example:443` also matches `https://docs.example/a`, but not
  `http://docs.example`, another hostname, or port `8443`.
- Existing redirects can reach another public origin without a second prompt.
  Explicit denial and private-address checks still apply to redirect hops.
  The original-origin grant does not authorize a separate direct call to the
  redirected origin. Explain this in the preview rather than claiming the entire
  fetch is confined to the granted origin.

### Subagents

A `spawn_subagent` grant allows any target agent type, task, and briefing. It
does not change existing spawn availability, agent selection, concurrency, or
depth limits, and it does not grant the spawned agent permission to use other
tools. The grant's **requester** scope is separate from the type of agent being
spawned.

## Scope and visibility

- Temporary grants default to the requesting agent instance. The user can
  explicitly share one with the root and its subagents, including later
  subagents of that root. They expire on a root session switch, `/clear`, or
  exit; do not restore them when a session is resumed or cloned.
- Persistent grants are scoped to the active profile and workspace, apply
  to every agent type and to both root agents and subagents, take effect
  immediately after a successful save, and by default also apply in headless
  runs. A different profile or workspace does not inherit them implicitly.
  Each agent's own effective policy is still evaluated first.
- Re-evaluate not-yet-executed calls when grants change, including calls already
  waiting in another agent's approval queue. Recheck the effective policy and
  requester scope before dispatch. Do not revive a cancelled call or interrupt
  an already-running one.
- Indicate on an automatically approved call whether a temporary or persistent
  grant was used. Record the final decision's source and a non-sensitive
  reference to the matching grant in structured logs, retaining the preceding
  approval-required policy verdict as its parent. Do not copy raw request
  payloads or sensitive rule literals into logs merely to explain provenance.

Coordinate this with the [approval verdict trail](approval-verdict-trail.md):
the grant evaluator is the decision maker, the temporary or persistent grant is
the rule origin, and the original policy verdict is its parent. The grants
feature can record this information before the broader verdict-trail idea is
implemented; that idea is not a prerequisite.

Do not add a grant-management TUI interface yet. Temporary grants expire as above;
persistent grants can be edited by hand and reloaded on the next launch.
External edits do not silently change the already-running session. A slash-command to
list and individually revoke runtime grants is a possible later enhancement.

### Per-invocation opt-out

Offer `--no-workspace-grants` for TUI and headless invocations. It disables
persistent workspace grants for the entire invocation, including native
subagents, without changing configured tool rules or CLI auto-approval flags.
Ordinary invocations continue to load eligible persistent grants by default.
One-time approvals and temporary grants remain available where interactive
approval is supported.

With the flag set, make persistent-grant creation unavailable in the approval
dialog and explain why; do not save a grant that cannot take effect. After the
normal trust check, skip loading and validating the disabled grant entries,
even if the trusted file is malformed. A later invocation without the flag
must reject an invalid grant file normally.

The flag does **not** bypass workspace trust: the selected profile's grant file
remains covered, and an untrusted or changed file can still block startup.
Fingerprinting must also work for malformed file contents. File access and
other trust errors remain errors.

Command-line flags do not automatically propagate to another Ylva process
started by Bash or another program. Such a process must receive its own
`--no-workspace-grants` flag; this option is not an OS isolation boundary.

## Persistence and trust

Store persistent grants in
`<workspace-root>/.ylva/profiles/<profile>/grants.toml`, alongside the existing
workspace profile layout. The file belongs to that workspace and profile; it
does not apply to a different profile or linked worktree merely because
sessions share a `project_key`. Load it only from the workspace; a missing file
means no persistent grants, not a fallback to a home-profile grants file.

`grants.toml` can be the only workspace profile file: it does not create or
shadow `config.toml`, so configuration can still come from the home profile
or, for the implicit default, built-in defaults. An explicitly selected profile
still requires an actual config file in a workspace or home layer. Reuse the
existing profile-name validation; creating a grant store is not profile
creation.

When a workspace profile directory contains grants but no workspace
`config.toml` or `.env`, `ylva profile list` should label it as grants-only
without claiming that the home profile configuration is shadowed. Do not
create a workspace shim solely because such a directory exists. An explicit
workspace profile deletion removes the directory and its grants file; the
command output, including dry-run output, and documentation must say so.

Creating its `.ylva/` directory also creates a workspace marker. At an
already-discovered Git root this does not move the root, but in an unmarked
directory it changes workspace discovery and configuration layering. If no
workspace marker was discovered, offer to create `.ylva/` in the launch
directory as part of saving a persistent grant. Show the proposed root and
warn about the new marker before confirmation. Create no profile config as a
side effect, and apply the saved grant to that workspace and profile in the
current run.

If the proposed `.ylva/` is the global Ylva home, including through a
canonical alias, do not write the grant there or treat it as a workspace.
Explain that persistent grants need a different workspace; keep one-time and
temporary approvals available. If saving fails after creating a marker, report
whether it remains and leave the approval pending.

A workspace-owned grant file must be covered by
[workspace trust](../architecture-design-records/0003-trust-workspace-configuration-by-content-hash.md).
Even though a grant cannot override an effective denial, an untrusted repository
must not be able to approve its own otherwise-prompted calls. For example, a
repository-supplied Bash grant could otherwise bypass a normal approval prompt
without changing any deny rule.

Include the selected profile's grant file in the covered content before loading
its grants and update the recorded decision. This must also work when the grant
file is the only workspace configuration file. The existing trust gate must
apply to both TUI and headless startup: an untrusted or changed file cannot
authorize calls. A confirmed grant takes effect in the current run after
saving, but saving must not silently re-trust unrelated workspace
configuration. The next launch requires `ylva trust`, including `--profile`
when appropriate. A profile-specific grant-file change invalidates that
profile's trust, not another profile's trust.

The workspace file may appear in Git status and can contain sensitive paths or
command prefixes. Warn before saving and let the user decide whether to share
or ignore it; do not alter ignore rules, Git configuration, or the index.
Another checkout or worktree must independently trust its workspace file.

### Illustrative TOML

One possible format uses one entry per tool. The containing file
identifies the profile; entries apply to all agent types and roles. A
multi-tool choice can produce multiple entries. The names and fields below are
only a sketch:

```toml
[[grants]]
tool = "read"
rule = "./notes.md"

[[grants]]
tool = "read"
rule = "./**/*.md"

[[grants]]
tool = "glob"
rule = "./docs"

[[grants]]
tool = "bash"
rule = "git log *"

[[grants]]
tool = "bash"
rule = "rm -f build.log"

[[grants]]
tool = "load_skill"
name = "planning"
rule = "./.agents/skills/planning/SKILL.md"

[[grants]]
tool = "web_fetch"
rule = "https://docs.example:443"

[[grants]]
tool = "web_search"

[[grants]]
tool = "spawn_subagent"
```

Here the read rules are relative to the workspace root, the `glob` rule matches its traversal root,
`git log *` allows one or more extra arguments (a bare `git log` needs its own exact rule), and
`rm -f build.log` allows exactly that command.
A `load_skill` entry includes both its skill name and source path. Persistent entries cover current
and future agent types and both roles, which the approval preview must make clear.

Require enough fields to determine the exact scope; missing or invalid fields
must not broaden it. When workspace grants are enabled, reject malformed files,
unknown fields, unknown tool names, and invalid matchers with a clear error
rather than silently accepting part of the file. The store needs matching
attributes only, not file contents, search terms, subagent task text, or URL
paths and queries. Literal paths and selected Bash prefixes are exceptions and
may themselves be sensitive.

The file has no version field, consistent with other Ylva configuration. If an
incompatible format is needed later, introduce versioning or a migration then
without silently reinterpreting existing grants.

### Saving and failures

- Show the exact persisted entry and target file before confirmation. Do not
  create or modify a profile configuration as an incidental fallback. Creating
  a separately chosen grant store is distinct from creating a profile.
- Preserve unrelated entries, comments, and existing permissions. Create new
  files with owner-only permissions where supported; keep existing permissions
  rather than silently changing them.
- Serialize Ylva's saves, compare the current file with the loaded or last
  saved snapshot, and report external replacement or edits. Publish atomically
  using the project's existing safe-write practices. As with existing profile
  saves, an atomic rename alone is not a compare-and-swap against an
  uncooperative external editor.
- On a failed or conflicting save, leave the approval pending and explain
  whether retry is safe. Let the user instead approve once or choose a
  temporary grant; do not silently substitute either. If publication may have
  happened before an error, reconcile the file and report any uncertainty
  instead of claiming a definite success or failure.

Before implementation, settle any remaining schema details and validate each
tool's rule using its own matcher.

## Runtime integration

Keep temporary grants in state shared by the root and its subagent loops, with
the requesting agent instance attached to each narrow grant. Persistent grants
need the active profile and workspace at evaluation, not an agent-type or role
filter. Tool registration and each agent's normal approval and execution checks
remain the authority for whether the call is otherwise allowed.

Grant updates must reach paused agents as well as future calls. Re-evaluate
matching waiting calls without misrouting their decisions to another agent or
leaving stale approval prompts visible. Session clearing, switching, and
cancellation must not allow an old pending call or temporary grant to leak into
the next root conversation.

## Acceptance scenarios

- Existing one-time approval and denial behave unchanged. An effective denial,
  invalid arguments, disabled tools, and private-address checks are not
  bypassed; existing CLI-flag and rule-list precedence remain unchanged.
- A temporary grant matches only its requesting instance unless explicitly
  shared. Shared grants cover eligible root and subagent calls, including
  already-waiting calls. Switching sessions, `/clear`, and exit remove them.
- A persistent grant matches its selected workspace, profile, tool, and
  resource for any agent type or role, including in headless mode. An
  agent-specific approval-required rule can be satisfied by the grant; an
  effective denial cannot. Manually edited saved grants take effect after
  restart, not silently during the current run.
- With `--no-workspace-grants`, no persistent grant applies to the root or its
  subagents, while configured approvals, one-time decisions, and temporary
  grants keep their normal behavior. Persistent-grant creation is unavailable.
  A separately launched Ylva process needs the flag independently.
- Trust still covers the grant file when disabled: untrusted or changed content
  blocks startup. A trusted malformed file is ignored with the flag but is
  rejected when grants are enabled.
- Exercise exact file versus directory scope; differing read ranges and write
  contents; search/glob roots; skill name and source; and the documented
  descendant-check limitations. Test component-aware `*` versus `**`, explicit
  hidden paths, root-relative paths from a nested launch directory, moved
  workspaces, literal expansion and glob characters, absolute paths, and
  relative targets that escape the workspace, including through symlinks, and
  reject the bare `**` catch-all as an invalid grant rule.
- Exercise Bash exact and suffix-wildcard matches, the one-or-more suffix
  (`make *` does not cover a bare `make`), argument boundaries, quoted literal
  wildcards, and the `unquoted_glob`/`unquoted_brace_expansion` carve-out.
  Exercise compound calls: one row per uncovered
  simple command, every command covered before the call runs, an `auto_denied`
  command denying the whole call, read-only rows for covered commands, the
  footnote and approve-once requirement for unanalysable parts, and the
  confirm action saving grants while running the call. Exercise the
  non-grantable table (inline-code interpreters, wrappers, destructive
  wildcards, env prefixes, shell-state commands), literal program matching
  (`/usr/bin/make`), and edited-matcher validation (program change, non-match
  including a suffix that no longer matches the row, newline, widening). An
  unrepresentable command remains approval-required.
- Exercise web origin scheme/host/effective-port matching, different paths and
  queries, backend changes for `web_search`, redirects, explicit denials, and
  private-address checks. A spawn grant must preserve existing spawn limits.
- An untrusted or changed grant file cannot authorize calls, including
  headlessly; saving never automatically re-trusts the workspace. Check linked
  worktrees and profiles independently. Verify that a grant file can exist
  alone without shadowing home or implicit built-in configuration, and that
  explicit-profile validation is unchanged. Check grants-only profile listing,
  shim generation, and explicit deletion including dry-run output.
- In an unmarked directory, require confirmation before creating the workspace
  marker, do not create a profile config, and apply the saved grant in the
  current run. Do not treat the global Ylva home as a workspace. Report a
  marker left behind by a failed save.
- Check malformed stores, missing files, duplicate grants, external edits,
  concurrent saves, write failures, uncertain publication, permissions, and
  previews of sensitive literal values. Do not change Git ignore rules or the
  index. Failed saves must not produce a success-shaped persistent grant.
- Check that an edited rule matches the current request, and that the final
  approval's grant origin and parent policy verdict are logged without raw
  request payloads or sensitive rule literals.

When implementing, update `docs/features.md`, `docs/configuration.md`, and
`docs/persistence.md`, `docs/cli-options.md`, and the workspace-trust ADR;
update `docs/keybindings.md` for the per-row `off | temporary | persistent`
selector and matcher editing, and any other new controls. Keep the static
non-grantable table in one code location, covered by tests, and document it in
`docs/configuration.md`.
