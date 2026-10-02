# Context usage and costs

## Context usage

Show the estimated tokens in the active model context and the configured
context limit. The estimate is approximate and exists to explain compaction,
not to represent provider-billed tokens. Do not persist usage history.

## Runtime costs

Track the cost of completed model requests and accumulate a run total while
the TUI or CLI is active. Do not store costs in session data. If the provider
does not report usage or pricing, leave that request out of the total rather
than estimating a precise billed amount from the context-token counter.

Display the accumulated total in the TUI footer, right-aligned on its second
row. The currency and pricing source still need a decision because providers
may not report comparable values.

When Ylva exits, print the accumulated total if it is available:

```text
Session: <session_id>
Costs:   <currency> <costs>
Resume with: ylva --conversation-id <session_id>
```
