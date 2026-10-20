# Redact sensitive info

We have known file `.env` in ylva's profile directories.
We can filter any tool call result for values of those sensitive
envvars and redact them, as a best-effort defense in depth that reduces the
chance of them reaching an LLM. This is not a guarantee of prevention.

We could even try simple heuristics of injected whitespaces, base64 encoding
or alike. Not sure how much security that would add, because there are
arbitrary ways one could circumvent this behaviour by introducing custom encoding
of a sensitive value ( knowing how ylva scans tool result output ).

`.env` may contain normal and sensitive envvars.
We should find a way to distinct normal from sensitive envvars and only redact
the sensitive ones!

How to select sensitive variables remains an unresolved design question.
Maybe we use a toplevel config property `redact_envvar_pattern` that could
be a regular expression that identifies envvars that are sensitive ?

## Relation to bash auto-approval

Redaction reduces the risk of auto-approving "readonly" `bash` commands
(see [bash-approval](bash-approval.md)), but is not sufficient on its own:
it only knows values from the profile `.env`, cannot see transformed
values, and does not cover exfiltration that bypasses tool results.
