# Bash rules

Allow basic rules to allow/deny simple executable calls included
in a bash tool call.

The new sandbox functionality (current implementation of
`SandboxContext::check_command` is just a dummy, always abstaining with
`None`) should be able to extract all the commands included in an
arbitrary bash script and evaluate each one against configured rules.

## Extraction backend

Use [`tree-sitter-bash`](https://crates.io/crates/tree-sitter-bash)
(MIT-licensed, actively maintained, the same grammar used by GitHub code
navigation/Neovim/Helix) to parse the script into a real AST, rather than
hand-rolling a shell tokenizer/scanner.

Rationale: shell operators (`;`, `|`, `&`, `&&`, `||`) don't require
surrounding whitespace (`foo;bar` is valid), so any word-splitting-only
approach (e.g. `shlex`) can misclassify an unspaced injection
(`curl http://x;rm -rf /`) as a single opaque argument — which could let a
permissive rule (e.g. `curl **`) auto-approve a compound command whose
second half was never analysed. A real grammar distinguishes these node
types unambiguously; this rules out that entire bug class rather than
depending on hand-written quote-tracking. `conch-parser` (real shell AST,
MIT/Apache-2.0-compatible) was considered and rejected as unmaintained
since 2019; `yash-syntax` (actively maintained) was rejected as
GPL-3.0-or-later, incompatible with ylva's Apache-2.0 license.

Trade-off accepted: `tree-sitter` requires a C compiler at build time
(`cc` build-dependency compiling generated `parser.c`) — ylva has no
native build dependency today.

## Scanning scope (v1)

Walk `list` (`;`/`&&`/`||`/newline), `pipeline` (`|`), and
`simple_command` AST nodes only. Any other node type encountered anywhere
in the tree — redirections, subshells, command substitution (`$(...)`,
backticks), parameter/variable expansion inside a word (`$FOO`,
`${FOO}`), `if`/`for`/`while`/`case`, function definitions, heredocs —
makes that command (and only that command; see aggregation below)
un-analysable.

Examples of in-scope scripts:

- `find . -type f ; echo "Foo" ; cat foo.txt`
- `find . -type f | sort | cut -d "_" -f2`
- `git fetch && git rebase origin/main || echo failed`

## Fallback semantics

`None` universally means "no static verdict — fall through to the tool's
own default (`UserApprovalRequired` for `bash`)". It is never used to
imply auto-approval or auto-denial; it is the safe, always-ask-a-human
fallback for anything the extractor can't confidently resolve (variable
references, command/parameter substitution, unsupported syntax, parse
errors).

## Verdict aggregation

Each extracted `simple_command` is evaluated independently against the
configured rule list. The bash tool call's overall verdict is the
worst case across all extracted commands, ranked
`AutoDenied` > `UserApprovalRequired` > `AutoApproved`. An
un-analysable command contributes at least `UserApprovalRequired`.

## Backgrounding (`&`)

Trailing `&` (backgrounding) is handled as a new **named structural
rule**, alongside the existing `outside_workspace`/`dot_path` rules
(`docs/plans/phase10.3-sandbox-tool-rules.md`): default-floor verdict
`AutoDenied`, overridable per-tool via config exactly like the other two
named rules — not a hardcoded, non-overridable block.

## Unquoted shell globs in analyzed commands

`tree-sitter-bash` parses `cat *.txt` as two plain word nodes (`cat`,
`*.txt`) — it does not perform pathname expansion (globbing), since that
is a runtime, filesystem-dependent operation, not a syntactic one. A
`CommandGlob` rule matching on that static word count (e.g. `cat *`,
intending "cat with exactly one file argument") can therefore silently
diverge from what actually executes: if `*.txt` expands to several files
at runtime, `cat` receives multiple arguments, not the one the rule
author expected. This is the same class of problem as an unresolved
`$FOO` reference — the real value depends on state invisible to static
analysis — except it isn't flagged by walking for unsupported AST node
types, since `tree-sitter-bash` represents the glob characters as
ordinary text inside a normal word node. The AST walker must explicitly
inspect each argument word's text for this.

This only applies to **unquoted** occurrences — `cat "*.txt"`/
`cat '*.txt'` are not glob-expanded by real shells (quoting suppresses
pathname expansion), so a quoted literal stays safely matchable as-is.
`~` (tilde expansion) is intentionally *not* treated specially here.

Two more named structural rules (same mechanism and per-tool
overridability as `outside_workspace`/`dot_path`/`backgrounded`), kept
separate rather than merged into one, because the two character classes
have different real-world frequency/risk and a user may reasonably want
different policies for each:

- `unquoted_glob` — an argument word contains an unquoted `*`, `?`, or
  `[...]` (true pathname-expansion/globbing syntax, filesystem-state
  dependent). Default-floor verdict: `UserApprovalRequired`.
- `unquoted_brace_expansion` — an argument word contains an unquoted
  `{...,...}` (brace expansion). Note this is actually a purely
  textual/lexical expansion with *no* filesystem dependency (unlike
  `*`/`?`/`[...]`), so it is in principle statically enumerable — but
  implementing correct enumeration (nesting, `{1..10}` sequences) is
  out of v1 scope, so it defaults to the same conservative
  `UserApprovalRequired` floor for now. Default-floor verdict:
  `UserApprovalRequired`.

Both are independently overridable per-tool via config, e.g. a user
comfortable with ordinary glob risk but wary of brace expansion (or vice
versa) can relax one without relaxing the other.

## Rule matching (`CommandGlob`)

Add a third variant, `CommandGlob`, to the existing `SandboxRuleConfig`
enum (alongside the path-only `Named`/`Glob` variants), usable in
`[tool.bash]` rule lists. A `CommandGlob` pattern is a sequence of
literal tokens matched positionally against a command's argv:

- No trailing wildcard (e.g. `git log`) matches only an argv of exactly
  that length — `git log --oneline` does **not** match.
- A trailing bare `*` (e.g. `git reflog *`) matches when argv has
  **exactly one** more token after the literal prefix.
- A trailing bare `**` (e.g. `git branch **`) matches when argv has
  **one or more** extra tokens after the literal prefix.

`*`/`**` are only meaningful as the last token in a pattern; there is no
support for embedded/mid-pattern globbing in v1 (e.g. `feature/*` as part
of a literal token) — YAGNI, revisit if a real need surfaces.

## Config validation

Since `CommandGlob` shares the `SandboxRuleConfig` enum with the
path-only `Named`/`Glob` variants, config loading must fail fast with a
clear error if a tool's rule list contains the wrong kind for that tool
(e.g. a `Named`/`Glob` path rule under `[tool.bash]`, or a `CommandGlob`
rule under `[tool.read]`) — naming the tool, the rule's list index, and
the expected kind. A silently-ignored mismatched rule in a
safety-relevant sandbox config is a footgun.

## Later addition: `tirith`

[`tirith`](https://github.com/sheeki03/tirith) (see
[shell-hardening-with-tirith](./shell-hardening-with-tirith.md)) is
compatible with, and complementary to, this idea rather than competing
with it:

- This idea's `CommandGlob` rules are a deterministic, config-driven
  **shape matcher** (does this command's argv match a pattern the user
  wrote). `tirith` is a heuristic/threat-intel **content scanner**
  (homograph domains, base64 decode-exec chains, data exfiltration,
  pipe-to-shell) that can catch a malicious command whose argv shape
  still matches an otherwise-legitimate rule (e.g. `curl **` matching a
  homograph-domain URL).
- Must be invoked as an **external subprocess** of the `tirith` binary
  (e.g. `tirith check -- <command>`, a one-shot non-interactive check),
  never as a linked Rust crate: both `tirith` and its engine library
  `tirith-core` are AGPL-3.0-only on crates.io, incompatible with ylva's
  Apache-2.0 license if linked. Subprocess invocation is mere
  aggregation and avoids that concern — same posture as ylva's existing
  optional-external-tool checks (`which git`/`rg` in
  `docs/blueprint.md`); detect via `which tirith` and skip if absent.
- Additive-restrictive only: a `tirith` finding may make this idea's
  verdict *stricter* (escalate toward `AutoDenied`/`UserApprovalRequired`)
  but must never upgrade a `None`/`UserApprovalRequired` result to
  `AutoApproved`.
- Two integration points, not mutually exclusive:
  1. A veto layer applied even to commands this idea's rules would
     `AutoApprove` (closes the "matches an allowed shape but is
     semantically malicious" gap).
  2. Enrichment for commands that fall back to `None` here — surfacing
     `tirith`'s findings alongside the human-approval prompt, without
     changing the safe default.

## `CommandGlob` rules and unquoted glob/brace tokens (resolved)

A rule pattern's own `*`/`**` wildcard syntax has no awareness of whether
the argv token it matched is itself an unquoted shell glob (e.g.
`*.txt`) that undergoes filesystem-dependent pathname expansion at
runtime. As written, `cat *` would match both `cat notes.txt` (safe,
literal) and `cat *.txt` (risky — real expansion at execution time)
equally, and a more specific `CommandGlob` override placed after the
general `unquoted_glob` named rule (see "Unquoted shell globs in
analyzed commands" above) would silently bypass that check via
last-match-wins, defeating its purpose.

Decision: **positional abstention, and only for the single-token `*`
wildcard.** A `CommandGlob` pattern's `*` and `**` tail wildcards make
different promises about argument *count*: `*` promises "exactly one
extra argument," `**` promises "one or more." An unquoted glob/brace in
the matched tail token can silently violate the `*` promise — turning a
reviewed "exactly one file" op into a bulk multi-file op the rule
author never approved (materially different blast radius for a
mutating command, e.g. `git add *` becoming `git add` of many files).
`**` makes no such count promise — the author already accepted
arbitrary tail content (any number of arbitrary tokens, including far
more invasive ones than a glob), so an unquoted glob resolving to many
real argv entries at runtime is already inside what `**` accepted; it
is not a new risk class, and abstaining there would only needlessly
downgrade an already-intentionally-broad rule with no security benefit.

So: a `CommandGlob` rule whose trailing wildcard is `*` abstains (never
matches) when that one matched tail token contains an unquoted
glob/brace character, deferring to `unquoted_glob`/
`unquoted_brace_expansion` for such commands. A rule whose trailing
wildcard is `**` is unaffected by this check and matches normally
regardless of unquoted glob/brace characters in its tail tokens.
Literal-prefix tokens (for both `*` and `**` patterns) are always
matched by exact text equality and are unaffected either way, since an
exact-text comparison against a fixed pattern carries no count
assumption for the runtime expansion to violate — e.g. `git commit -m
*` still matches `git commit -m "*.txt is gitignored"` even though that
literal message argument happens to contain glob characters.

This abstention is an inherent, non-overridable part of `CommandGlob`
matching — not a new named structural rule with its own config floor.
Making it independently toggleable per tool would just recreate the
same last-match-wins bypass under a different name; the only sanctioned
way to relax risk here is loosening `unquoted_glob`/
`unquoted_brace_expansion` themselves. The same rule applies identically
to both unquoted globs (`*`, `?`, `[...]`) and unquoted brace expansion
(`{...,...}`) — no asymmetry between the two for v1, consistent with
brace enumeration itself being out of v1 scope (see "Unquoted shell
globs in analyzed commands" above).

## Out of scope for this idea (deferred, not yet planned)

- Turning this into a concrete `docs/plans/*.md` implementation plan and
  an ADR (structural rules stay a Rust-code floor; scanning scope is a
  fixed, documented subset with safe fallback) — follow-up step once
  prioritized.
- Variable-length/mid-pattern wildcards, and any relaxation of the v1
  scanning scope (subshells, redirections, substitutions).

See [shell-hardening-with-tirith](./shell-hardening-with-tirith.md) for a
related follow-up idea.
