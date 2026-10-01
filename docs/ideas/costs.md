# Costs

We should track what cost occur and accumulate them while the TUI / CLI
is running.

Don't store costs in the presisted session data, just accumulate at runtime.

Display the accumulated total costs in TUI footer ( second row, right aligned ),
always assuming $ costs ( currency always seems to be dollar, may be different
depending on LLM provider ? )

When Ylva CLI or TUI exits, it prints the accumulated costs:

```text
Session: <session_id>
Costs:   $ <costs>
Resume with: ylva --conversation-id <session_id>
```
