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

### If it were ever pursued: macOS Seatbelt design notes

Seatbelt is the macOS kernel-enforced process sandbox used for per-process
policies. Its policy language, SBPL, is a TinyScheme dialect. A profile can start
with default denial and selectively allow required operations; for example, an
allow for writes beneath a workspace followed by a deny for other writes. Policy
surfaces include file reads and writes, outbound network, process execution,
Mach service lookup (which can mediate desktop/browser control), signals and
other operations. This is distinct from the entitlement-based App Sandbox, which
requires signing/provisioning and applies at the app level.

The key properties for a shell tool are that confinement is **irreversible**
(the confined process cannot widen its policy) and **inherited** (descendants
remain confined). This could constrain what commands actually access, unlike
operand checks or executable drift observations. A command cannot escape merely
by changing `PATH` or shell state. The harness must choose carefully where the
boundary is applied:

- `/usr/bin/sandbox-exec -p <profile> -- <command> ...` runs the target in a
  separate sandboxed process, leaving Ylva unrestricted. Apple marks the CLI
  deprecated, though it still ships and is used by Chromium and others.
- The `libsandbox.dylib` C API (`sandbox_init` /
  `sandbox_init_with_parameters`) applies the profile to the calling process and
  all descendants. Because this is irreversible, Ylva would need to fork first or
  use a helper that applies the sandbox and then execs the command if the harness
  must stay unrestricted. The library remains available, but the API is also
  marked deprecated in headers and is a private/formally unsupported surface.
- A per-command helper is an alternative when another process owns spawning,
  including async process APIs; the helper can establish its own process group so
  cancellation can terminate the command tree.

The implementation trade-off is not simply CLI versus FFI. Keep policy generation
and execution separated: generate/validate profiles, run one command under the
profile, capture exit status and sandbox violations, and attach policy/backend
provenance to the result. Candidate Rust approaches include:

| Approach | Relevant trade-off |
| --- | --- |
| `procage` | Uses `libsandbox` on macOS and Landlock + seccomp + namespaces on Linux; offers child spawning, async support, violation reporting and policy receipts. It is pre-1.0, so its API may change. |
| Microsoft MXC (`mxc-sdk`) | JSON filesystem/network policy with Seatbelt on macOS and platform backends elsewhere; includes audit/learning mode to observe accesses and help converge on a policy. Its Seatbelt profile builder is also available separately. |
| Per-command `sandbox-exec` wrappers | Keep the harness unrestricted and can be lightweight, but depend on Apple's deprecated CLI and require profile generation/maintenance. |
| Raw `sandbox_init` FFI (`nono` is a reference) | Avoids a helper CLI and can be minimal, but inherits the irreversible in-process boundary and private API risk. |
| Cross-platform sandbox crates | Options such as `ai-sandbox` aim to map OS-specific mechanisms (Seatbelt, Bubblewrap/seccomp/Landlock, Windows restricted tokens, Capsicum or pledge) behind one API; support and semantics still differ by OS. |

Other Rust projects illustrate the range: `mxc-alpha-seatbelt-common` focuses on
profile building and a Seatbelt runner; `byob-sandbox`, `crabpot-macos`,
`harness-rs-sandbox`, `wardstone` and `sandbox-runtime` wrap generated or
hand-written SBPL with `sandbox-exec`. A hand-rolled SBPL profile plus
`std::process::Command` is the minimal-dependency option. These are research
candidates, not selected dependencies.

Two operational ideas are particularly relevant to Ylva even without adopting a
sandbox. First, violation reporting can turn denied operations from opaque
`EPERM` failures into explainable records (operation, process, target and rule),
which could feed the [approval verdict trail](approval-verdict-trail.md) and
make compatibility failures actionable. Second, audit/learning mode can record
observed access and help discover necessary exceptions before enforcement. Such
observations support policy authoring; they are not proof that a policy is safe or
complete. Policy digests, backend/tier and any relaxations should accompany
results for provenance.

Caveats: deny-by-default profiles need tuning because ordinary tools and
toolchains may read system program files and may need temporary, cache, lock or
state writes; network access is commonly denied unless explicitly allowed. Every
exception increases policy complexity. Path rules alone do not settle hard-link
and symlink risks or prevent secrets from being exposed through another allowed
channel; link/hand-off audits and network controls may be needed. SBPL behavior
must be tested across supported macOS versions and architectures (including
ARM64/x64). A macOS-only Seatbelt implementation is not a cross-platform
boundary; Linux, Windows and other systems need their own enforcement backend and
must not be assumed semantically identical.

## Learned persistent rules

See [reusable tool-approval grants](tool-approval-grants.md) for user-approved
temporary and persistent Bash grants and their interaction with existing rules.

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

## Improved grammar

Maybe improve/add grammar for further commands or propose modified bash rules
in `tests/sandbox_evaluation/sample.config.toml`.

- `xxd`
- `base32` and `base64`

## Grammar coverage and adversarial validation

Extend the grammar roster only when repeated approvals demonstrate a useful
read-only subset. `awk` remains a potential narrowly scoped addition;
its program operands need dedicated validation, not just option allow-lists.
User-defined grammars are a possible later extension, not a prerequisite.

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
