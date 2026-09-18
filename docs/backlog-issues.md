# Backlog of issues discovered while testing

- [x] Exhibited subagent failing to use
      ```text
      ▶ Agent Finish ↵The code search ...
      "status" is a required property

      ▶ Agent Finish ↵The code inspection ...
      "status" is a required property
      ```
      See log tests/e2e/custom/logs/tui_20260918185619.log

      Root cause: a weak local model (`gemma4:e4b-mlx`) repeatedly omitted the
      required `status` field on `agent_finish`, causing auto-denials and
      wasted retries. Fixed by making `status` inferable from `error`/`result`
      when omitted (see `src/agent/tools/agent_finish.rs`), and by
      strengthening the tool description to state `status` is mandatory and
      should be emitted first.
