# Bash approval: remaining gaps and enhancements

This document keeps only deferred ideas. For the implemented approval contract,
see [`bash` command rules](../configuration.md#bash-command-rules).

The remaining distinction is between an **approval convenience** and an enforced
execution boundary: argument-aware rules cannot guarantee read-only or harmless
execution when executables, environment, configuration and filesystem state remain
uncontrolled.

## Executable drift detection

Bare command names do not identify trusted binaries. A workspace executable or a
user-writable directory in `PATH` can supply an evil twin of `cat`, for example.
Inherited shell state such as `BASH_ENV`, exported functions and tool-specific
environment variables can also change execution without appearing in argv.

Bash selects and executes commands; Ylva does not select each executable in a
script. Explore a best-effort **drift check**, not executable pinning:

- Capture Ylva's startup `PATH` and discover commands eligible for auto-approval
  against that baseline. Record canonical executable paths and content hashes.
- Before auto-approval, repeat discovery using the captured `PATH` and compare
  canonical paths and content hashes with the startup baseline.
- Require approval when a path or content hash changes, or when resolution,
  canonicalization or fingerprinting fails or is ambiguous.

Prefer Bash's `command -v` over external `which` for discovery: `which` generally
searches `PATH` without representing Bash's full command resolution. A function,
builtin or other result without an identifiable executable file cannot be treated
as a successful file comparison. Lookup context and baseline handling for these
cases still need design.

### Explain withheld auto-approval

A detected change must produce a specific explanation in the approval prompt,
not silently fall back to a generic request. For example:

- "Auto-approval withheld: `cat` now resolves to `/new/path/cat`; at startup it
  resolved to `/usr/bin/cat`."
- "Auto-approval withheld: the contents of `/usr/bin/cat` changed since startup."

Report failed or ambiguous checks explicitly as well. Refer to an earlier
auto-approved execution only if that execution was actually recorded; otherwise
describe the comparison against the startup baseline. Verdict provenance and
explanations are part of this enhancement, not an optional extra.

### Limits of the claim

Startup `PATH` and its executables are not necessarily trustworthy. Matching paths
establish only the same resolved location; content hashes additionally detect
changed bytes, not whether those bytes are safe.

A separate lookup cannot prove what a later script executes. Files can change
between checking and execution, and earlier commands in the script can change
`PATH`, functions or other shell state. Tool configuration, pagers, plugins and
helpers can also change what runs. These remain outside the guarantees of the
drift check; enforcing execution restrictions is outside this proposal's scope.

The claim is only that Ylva detects observed resolution or content changes against
its startup baseline and withholds auto-approval. An unchanged result is not proof
of read-only execution or unchanged behavior.

## Out of scope: Bash file-access limitation

Bash operands are not checked against the path rules used by dedicated tools.
An auto-approved read-style Bash command can therefore read outside the workspace
or access secrets such as `~/.ssh/id_rsa`, credentials or another project's `.env`.

Bash operand checks cannot establish an access boundary: shell state, expansion
and filesystem changes affect which files are accessed. Earlier commands can
change that state, and even literal absolute paths can resolve differently after
filesystem changes.

**Do not pursue operand path scoping as an auto-approval mechanism.** Its limited
veto value does not justify grammar-level path classification without establishing
the paths actually accessed. Prefer dedicated `read`, `glob` and `search` for
scoped access. Enforcing Bash file-access restrictions is outside this proposal's
scope.

## Out of scope: OS-enforced execution isolation

OS-enforced isolation is outside this approval-convenience proposal. It would
introduce a separate execution model, platform-specific maintenance and
compatibility problems: even read-style commands may need writable temporary,
cache, lock or state paths. Restricting those writes can cause unexpected tool
failures; allowing exceptions adds policy complexity.

Such a security boundary would require separate product justification, design and
an architecture decision record. It is not a planned enhancement here. Bash
auto-approval does not promise read-only execution.

## Learned persistent rules

Offer an explicit user-owned "always allow this command prefix" action when
approving a command, for example `git log`. Persist a narrowly scoped rule rather
than learning a broad command-name wildcard.

Reuse grammar knowledge to distinguish subcommands, options, values and operands.
Decide how learned rules interact with existing rule order, configuration layers,
agent types and later grammar changes. The approval UI should preview the exact
scope being persisted.

## Verdict provenance and explanations

Carry structured decision provenance through approval evaluation and show it in
the approval prompt: matched rule, configuration origin, relevant argument
identity, unsupported syntax or the point where parsing became ambiguous.

See [approval verdict trail](approval-verdict-trail.md). An aggregate verdict
should retain the contributing command and reason, not only the final status.
This would also provide evidence for ranking frequently approved commands before
extending grammars or suggesting persistent rules.

## Additional veto layers

- Evaluate optional heuristic scanning via
  [shell hardening with `tirith`](shell-hardening-with-tirith.md). Keep it an
  external subprocess and an additive restriction, never an approval source.
- An LLM judge could provide additional warnings for ambiguous commands, but is
  prompt-injectable, unreliable and adds latency. Consider it only as a veto or
  explanation aid, never as authority to auto-approve.

## Grammar coverage and adversarial validation

Extend the grammar roster only when repeated approvals demonstrate a useful
read-only subset. `sed` and `awk` remain potential narrowly scoped additions;
their program operands need dedicated validation, not just option allow-lists.
User-defined grammars are a possible later extension, not a prerequisite.

- Grammar enhancement for commands like `sed -n 825,845p <path>`
- etc.

Further validation could include:

- A curated GTFOBins/LOOBins bypass corpus covering execution, writes and
  exfiltration, replayed against the actual approving rules.
- Broader fuzz/property testing of shell extraction and argument parsing,
  including cross-platform option conflicts and sequences of shell-state changes.
- Re-reviewing supported spellings against new OS/tool releases before expanding
  accepted forms.

These enhancements should build on the existing parse cases, generated
unknown-arity/bundle checks and
[offline Bash policy replay runner](../../tests/sandbox_evaluation/README.md),
not duplicate them. Recognizing an option is not approving it; evaluate bypass
expectations against the complete policy.

## Complementary sensitive-output redaction

[Redact sensitive info](redact-sensitive-info.md) proposes filtering sensitive
values from tool results; it is not an implemented protection to rely on.
Even if added, it would not enforce filesystem or execution restrictions:

- Secrets outside the selected profile environment need separate coverage.
- Transformed values can evade literal matching.
- Exfiltration may bypass tool results entirely.
- Execution-time side effects occur before result redaction.

## References

- [GTFOBins](https://gtfobins.org) and its
  [source/data](https://github.com/GTFOBins/GTFOBins.github.io): candidate bypass
  corpus for Unix binaries.
- [LOOBins](https://www.loobins.io): macOS techniques.
- [LOLBAS](https://lolbas-project.github.io): Windows techniques.
- Potential prior art to verify before adoption: OpenAI Codex CLI, Claude Code
  permission/sandbox documentation and OpenCode permission-prefix handling.
