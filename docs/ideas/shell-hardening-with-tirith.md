# Shell hardening with `tirith`

Use [tirith](https://github.com/sheeki03/tirith) as an additional check
when running auto-approve logic for the `bash` tool.

`config.toml` should be enhanced with a global setting like `tirith_enabled=true|false`,
where default is disabled.
Maybe we should add a setting to select the executable path like
`tirith_executable=<command>`, which will default to just `tirith`
( allowing it to be found via `${PATH}` environment ).

Should the configuration be like the following ?

```toml
[tirith]
enabled = "true"               # default: "false"
executable = "/usr/bin/tirith" # default: "tirith"
```

## What it is

`tirith` is a Rust CLI that performs heuristic/threat-intel scanning of
shell commands: homograph-attack hostnames, pipe-to-shell patterns
(`curl | bash`), base64 decode-execute chains, data exfiltration
(`curl -d @/etc/passwd`), credential detection, and 200+ other rules
across ~29-35 categories.
`tirith` can flag a command whose argv shape matches an otherwise-legitimate
rule (e.g. `curl **`) but whose actual URL/arguments are malicious (a homograph
domain, an exfiltration flag), a class of attack no shape-only glob rule
can ever catch.

## Licensing constraint: subprocess only, never a linked crate

Both the `tirith` CLI crate and its underlying engine crate,
`tirith-core`, are published on crates.io as **AGPL-3.0-only**. Linking
either into ylva (Apache-2.0) would require ylva to relicense — not
viable. Integration must go through the **compiled `tirith` binary as an
external subprocess** (mere aggregation, not linking), the same posture
ylva already takes for other optional external tools (`which git`/`rg`
checks in `docs/blueprint.md`).

`tirith` exposes exactly the one-shot, non-interactive API this needs —
no shell-hook/`eval "$(tirith init ...)"` setup required for
programmatic use:

- `tirith check -- <command>` — analyze a single command string without
  executing it; returns findings with severity (`CRITICAL`/`HIGH`/
  `MEDIUM`/`LOW`/`INFO`).

Detect availability via `which tirith`; skip the check entirely (no
behavior change) if the binary isn't installed — this must remain an
optional, additive layer, not a hard dependency.

## Integration shape (additive-restrictive only)

A `tirith` finding may only make the sandbox's verdict for a `bash`
command **stricter** (escalate toward `AutoDenied`/
`UserApprovalRequired`); it must never upgrade a `None`/
`UserApprovalRequired` result to `AutoApproved`. Two integration points,
usable independently or together:

1. **Veto layer on top of `CommandGlob` rule evaluation** — run `tirith check`
   even on commands that would otherwise be `AutoApproved`, to close the
   "shape matches, content doesn't" gap described above.
2. **Enrichment for `None`-fallback commands** — for command rules
   can't statically analyze (and which therefore always require human approval),
   surface `tirith`'s findings alongside the approval prompt as extra context,
   without changing that safe default.

## Status

Idea only — not yet scoped into a `docs/plans/*.md` implementation plan.
Depends on `CommandGlob` existing first, since that's
what establishes the `check_command` extraction/verdict pipeline this
would hook into.
