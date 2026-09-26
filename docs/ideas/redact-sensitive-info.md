# Redact sensitive info

We have known file `.env` in ylva's profile directories.
We can filter any tool call result for values of those sensitive
envvars and redact them, so they can never reach an LLM.

We could even try simple heuristics of injected whitespaces, base64 encoding
or alike. Not sure how much security that would add, because there are
arbitrary ways one could circumvent this behaviour by introducing custom encoding
of a sensitive value ( knowing how ylva scans tool result output ).
