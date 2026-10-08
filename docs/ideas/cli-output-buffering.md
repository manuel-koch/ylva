# CLI output buffering

The headless CLI currently streams assistant text to stdout as it arrives.
Consider buffering text until completion; JSON output should be emitted only
after the run finishes.

## Open questions

- Decide whether text mode should remain live or buffer until the response
  completes.
- If text is buffered, define how partial assistant output is exposed after an
  interrupt or provider failure.
- Bound memory use for long responses, or stream to a temporary file if a
  bounded buffer is required.
- Coordinate the text and JSON rules with
  [`cli-output-streams.md`](./cli-output-streams.md).

## Related

- [`cli-json-output.md`](./cli-json-output.md)
- [`cli-exit-codes.md`](./cli-exit-codes.md)
- [`docs/features.md`](../features.md#cli-mode)
