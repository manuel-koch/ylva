# Concurrent subagent execution with a configurable limit

## Context

**Component changed:** Ylva's child-agent (subagent) scheduler and the code paths that assume at
most one child runs at a time: `ChildAgentManager`, the root agent runtime, the child-to-parent UI
forwarding, and the TUI view model. Configuration gains one new setting.

**Current behavior:** Subagents run one after another. At most one child is active alongside the
main agent. Further `spawn_subagent` calls are queued in FIFO order and started when the preceding
child finishes.

**Goal:** Allow subagents to run concurrently with the main agent and with each other. Add a
customizable setting that caps how many children may run at the same time across the whole agent
tree. Queued spawns beyond the cap wait in FIFO order.

**Motivation (from the idea note `docs/ideas/agent-concurrency.md`):** "It should be possible to
run some/all subagents concurrently. We should introduce a customizable concurrency setting to limit
how many subagents can concurrently run with main agent."

### Acceptance criteria

- A new per-profile configuration setting caps the number of concurrently running child agents. The
  default is `4`. A value `<= 0` means unlimited.
- The cap is global across the whole agent tree. Root plus all nested children count toward the same
  cap.
- With a finite limit `N`, at most `N` children are in the running state at any moment. Spawns beyond
  `N` are queued and start in FIFO order as running children finish.
- With limit `1`, observable behavior matches today's behavior: one child at a time, FIFO queue,
  the same `subagent_accepted` statuses (`running` / `queued`), and the same single re-prompt of the
  root agent after the last outstanding child finishes.
- With unlimited, every spawn starts immediately.
- The root agent is re-prompted exactly once, after `outstanding_count == 0`, even when several
  children finish at different times.
- Every child's streamed text and thinking is attributed to that child's `run_id` in the TUI, even
  when several children stream at the same time.
- Tool approvals and clarify requests raised by a child are routed back to that child by `run_id`,
  even when other children are running.
- Approval prompts are shown one at a time, in the order they were requested. A child whose prompt is
  waiting stays blocked. Other running children continue.
- `Stop` and `ClearContext` cancel all running and queued children, and each cancelled child yields
  its own cancelled `FinishResult`.
- The TUI footer `Subagents: N running, M queued` is correct for N > 1.
- `make format lint` and `make test-coverage` pass (coverage at least 80%).
- Documentation that currently says "at most one child at a time" is updated.

### Key constraints

- The runtime is tokio. Children are `tokio::spawn`ed. Channels are `tokio::sync::mpsc`. Per-child
  cancellation uses `tokio_util::sync::CancellationToken`.
- The root loop must never `await` a send on the channel it drains itself. Child UI and finish
  channels are unbounded, so every send from the scheduler and the child driver is synchronous and
  never blocks or drops an update. Each child's private finish channel is capacity 1 and is awaited
  only by its own driver task, never by the root loop.
- Provider HTTP concurrency is already bounded per provider connection by
  `RequestConcurrencyLimiter` (`MAX_CONCURRENT_PROVIDER_REQUESTS = 4`). The new limit bounds
  children only. It does not replace the provider limiter.
- Each child has its own `Context`, `Sandbox`, and tool registry. Children share the parent `Store`
  through `Arc<Mutex<Option<Store>>>`. Child transcripts are written at completion.
- Children share one filesystem. Concurrent children may write the same file. This is accepted and
  documented, not serialized.
- Coding convention: use full, descriptive names. No abbreviations.
- Rust 2018+ file-per-module layout (`foo.rs`, not `foo/mod.rs`).
- Lint suppression, if ever needed, uses `#[expect(...)]` on the specific item, never `#[allow]`.
- No ADRs exist yet in `docs/architecture-design-records/` (only `.gitkeep`). No recorded decision
  constrains this change.

## Implementation Plan

Execution protocol: an implementing agent must flip `[ ]` to `[x]` immediately after completing each
step and rewrite this file in place, so progress is resumable across sessions. Complete the steps in
order. Verify each step with its named tests before moving on.

1. [x] Add the configuration setting `max_concurrent_subagents` to the profile, mirroring
   `input_history_limit`.
   - In `src/config/profile.rs`: add a field to `ProfileConfig` with serde default `4` and a
     deserializer that accepts an integer or a string form, following `deserialize_usize_or_string`.
   - Accept signed integers so that `<= 0` can be expressed. Store the value as
     `Option<usize>`, where `None` means unlimited. Map any value `<= 0` to `None`. Reject
     non-integer strings, as today.
   - Add an accessor on `ProfileConfig` that returns the effective `Option<usize>`.
   - Do not reject `0`. It means unlimited.
   - Add a `DEFAULT_*` constant with value `4`.
   - Tests in `src/config/profile/tests/`: default is `Some(4)`; explicit positive value parses;
     string form parses; `0` and negative values parse to unlimited (`None`); non-integer string
     is rejected.
   - Commit boundary: this step together with step 11 (documentation).

2. [x] Thread the limit into the child scheduler.
   - In `src/agent/child_agent_manager.rs`, add a `max_concurrent_children: Option<usize>` field, set
     in the constructor. `None` means unlimited.
   - Pass it from `AgentFactory::build_child_agent_manager` (`src/agent_factory.rs:573-574`).
   - Keep `spawn_fn` unchanged. It is already `Send + Sync`.
   - Tests: constructor stores the limit for finite and unlimited values.

3. [x] Replace the single running slot with a per-run collection in `ChildAgentManager`.
   - Change `running: Option<RunningChild>` to a map keyed by `run_id` (`HashMap<Uuid, RunningChild>`
     or an equivalent with deterministic iteration order for `cancel_all`).
   - Replace `is_running()` with a method that answers whether a slot is free: `true` when the limit
     is `None`, or when `running.len() < limit`. Use a full descriptive name, e.g. `has_free_slot()`.
   - `spawn`: if a slot is free, call `start` immediately. Otherwise enqueue and emit `queued` progress.
   - `start`: insert into the map. Emit `ChildAgentStarted` and `running` progress as today.
   - `on_child_finished(run_id)`: remove that run only. Ignore unknown run ids, as today. Then loop:
     while a slot is free and the queue is non-empty, pop and `start` the next request.
   - `outstanding_count()` becomes `running.len() + queue.len()`.
   - Tests in `src/agent/child_agent_manager.rs` (`tests` module, lines ~518-1002):
     - Update `spawn_returns_queued_when_a_child_is_already_running` (line ~677) and
       `multiple_queued_spawns_are_drained_in_fifo_order_after_finish` (line ~710) to run at limit
       `1`. Their assertions stay the same.
     - Add: with limit 3 and 5 spawns, exactly 3 start and 2 queue.
     - Add: finishing one of three running children starts exactly one queued child, in FIFO order.
     - Add: unlimited limit starts all spawns immediately.
     - Add: stale or unknown `run_id` is ignored and does not free a slot.
     - Add: `outstanding_count` equals running plus queued at each step.

4. [x] Replace single-child accessors with per-run lookups.
   - `running_run_id()` (`child_agent_manager.rs:371-373`): remove. Its only caller is the child
     token stamping in `src/agent/runtime/subagents.rs:273-297`. Replace that stamping with step 5.
   - `running_duration` (`child_agent_manager.rs:332-337`) and its caller at `subagents.rs:173-177`:
     make it take a `run_id`.
   - `running_command_sender(run_id)` (`child_agent_manager.rs:381-386`) already takes a run id.
     Make it a map lookup. Confirm that `ForChild` routing at `src/agent/runtime/commands.rs:650-667`
     resolves each child by run id.
   - `cancel_all()` (used by `Stop` at `commands.rs:376-440` and by `ClearContext` at
     `commands.rs:582-590`): iterate all running children, cancel each child's token, and emit one
     cancelled `FinishResult` per child. Also drain the queue as today. Record every cancelled
     `run_id` in `cancelled_run_ids` (`runtime.rs:54`).
   - Tests: `Stop` with two running and one queued child yields three cancelled completions and
     leaves `outstanding_count() == 0`.

5. [x] Attribute child output to its `run_id` at the source.
   - In `drive_child_to_completion` (`src/agent_factory.rs:961-1062`), the `forward_task` at
     `agent_factory.rs:970-976` tags each forwarded `AppendText` / `AppendThinking` with that child's
     own `run_id`, before the update enters the shared `subagent_ui_tx`.
   - Remove the `running_run_id()` lookup in `forward_subagent_update`
     (`src/agent/runtime/subagents.rs:258-321`). Emit `ChildAgentToken { run_id, .. }` using the run
     id carried by the update.
    - Tests: two children stream interleaved text; each `ChildAgentToken` carries the correct
      `run_id`. The source-attribution unit test is in
      `src/agent_factory/tests/children.rs`; the integration test
      `interleaved_child_tokens_retain_their_own_run_id` is in
      `tests/tools_agent_loop/subagents.rs`.

6. [x] Make lifecycle UI updates lossless at any volume.
   - The shared `subagent_ui` and `finish` channels are unbounded (`src/agent_factory.rs`,
     `build_child_agent_manager`). Their capacity therefore never depends on the configured limit,
     which can be unlimited, and no update is dropped.
   - Lifecycle sends at `child_agent_manager.rs` (`send_lifecycle_update`) are synchronous. A closed
     channel is logged at `debug!`, because it means the root has already exited.
   - Each child reports its finish on a private capacity-1 channel. The driver forwards that finish
     to the shared channel only after the child's UI forwarder has drained, so a child's last UI
     update always precedes its finish. The root loop drains queued subagent UI updates before it
     handles a finish.
   - Tests: `lifecycle_updates_are_never_dropped_under_volume` (1500 spawns, unlimited cap) in
     `child_agent_manager.rs`. `child_completion_is_delivered_after_the_child_tool_done_that_precedes_it`
     in `tests/tools_agent_loop/subagents.rs` covers ordering.

7. [x] Serialize approval prompts with a FIFO queue.
   - `ToolPending` and `ClarifyPending` already carry `run_id: Option<Uuid>`
     (`src/agent/protocol.rs:323-334`). The driver wraps replies in `AgentCommand::ForChild`
     (`commands.rs:650-667`).
   - Keep a FIFO queue of pending prompts. Show only the head in the TUI. When the head is answered,
     show the next. A child whose prompt is not at the head stays blocked on its approval, and other
     children continue.
    - Replace the single-slot `Focus::ToolApproval { tool_id, tool_name }`
      (`src/tui_reducer_state/events.rs:96`) with the queue head. Update `tool_id` checks in
      `src/presenter/domain.rs` and
      `src/tui_reducer_state/conversation.rs:716`.
   - Confirm that a reply to a finished child is dropped without hanging the root (existing test
     `for_child_targeting_an_unknown_run_id_is_dropped_without_hanging_the_root` in
     `tests/tools_agent_loop/subagents.rs:459`).
   - Tests: two children request approval at the same time. The second prompt appears only after the
     first is answered. Each reply resolves only its own tool call.

8. [x] Replace the TUI single-child slot with a per-run structure.
   - `running_child: Option<ChildRunInfo>` in `src/tui_reducer_state.rs:343` becomes a map keyed by
     `run_id`, or is removed if the per-run `ChildConversation` entries already carry the needed data.
   - Update `start_child_run` (`src/tui_reducer_state/conversation.rs:613-617`), the clear on
     `ChildAgentDone` (`src/presenter/domain.rs:161-166`), and `agent_mode_for_child`
     (`conversation.rs:731-737`).
   - Tests: under `src/tui_reducer_state/tests/` and `tests/tui_render_state/subagent_views.rs`,
     two children running at once each keep their own mode and conversation.

9. [x] Verify the footer and counts.
   - `ActiveSubagentCounts::from_entries` (`src/tui_reducer_state/conversation.rs:135-170`) and
     `active_subagent_counts()` (`conversation.rs:674`) already count per bubble. Confirm that the
     footer at `src/tui_widgets/footer.rs:627-636` shows `Subagents: 2 running, 1 queued` for a
     limit-2 run with three spawns.
   - Tests: footer rendering with multiple running children, in `tests/tui_render_state/subagent_views.rs`
     or the footer suite under `tui_widgets/`.

10. [x] Add integration tests for the observable contract.
     - In `tests/tools_agent_loop/subagents.rs`, using the controlled mock provider and
       `start_paused = true` as the existing tests do:
       - Limit 2, three spawns: at most two children are running at any time.
        The third starts when the first finishes.
      - Limit 3, three children: all three run concurrently. The root is re-prompted once, after the
        last child finishes.
      - Unlimited, five children: all five run concurrently.
      - Limit 1: the existing test `intermediate_subagent_completion_does_not_reprompt_until_all_children_finish`
        (line 18) passes unchanged.
       - `stop_cancels_multiple_running_and_queued_subagents` passes with two running and one queued
         child.
    - Coordinate asynchronous steps through observable events and explicit gates with bounded waits,
      not fixed sleeps (see `docs/development-workflow.md` → *Deterministic test dependencies*).

11. [x] Update the child-delegation documentation and the setting reference.
    - `docs/features.md`:
      - `spawn_subagent` section (lines ~685-738): replace "the current scheduler runs at most one
        child at a time" with the configured cap, its default, and the unlimited value.
      - Child-agent delegation section (lines ~740-773): same change. Keep "stopping cancels any
        running/queued children". Add that approval prompts are shown one at a time, and that
        concurrent children may write the same files.
    - `docs/implementation-notes.md` (child delegation internals, starting ~line 612): update the
      scheduler description, per-run attribution (step 5), approval queue (step 7), channel capacity
      (step 6), and the view-model note at line ~654.
    - `docs/configuration.md`:
      - Top-level profile table (around lines 100-106): add the new setting row, default `4`,
        `<= 0` means unlimited.
      - Example config (around lines 60-100): add a commented example.
      - Per-agent table (around lines 420-427): clarify that the cap is profile-level and global across
        the tree, and that the `can_spawn` description refers to concurrent children up to that cap.
    - `docs/keybindings.md` (lines ~17-18): verify the "Subagents: 1 running, 1 queued" example still
      reads correctly. Update it if it becomes misleading.
    - `README.md` (lines ~23-30): update any subagent description that says children run one at a time.
    - `docs/ideas/agent-concurrency.md`: after the feature ships, ask whether to delete it or move it
      to a shipped note. Do not delete it without consent.

12. [x] Add the CLI override `--max-concurrent-subagents <N>`.
    - Add the flag next to `--auto-approve-spawn` (`src/cli.rs:146, 564`), following the layered
      override pattern at `src/config/profile.rs:860-870`. `<= 0` means unlimited, as in the profile.
    - Tests: CLI override beats profile value; `0` yields unlimited.
    - Document it in `docs/configuration.md` and `docs/features.md`.

13. [x] Run the verification gate.
    - `make format lint` must pass. Fix every warning.
    - `make test-coverage` must pass with at least 80% aggregate line coverage.
    - The documentation named in step 11 is updated in the same commit as the code it describes.

## Proposed Improvements

- **Use a semaphore instead of a hand-counted slot.** `RequestConcurrencyLimiter` in `src/http.rs:77-98`
  already wraps a `tokio::sync::Semaphore`. A child-slot semaphore, with permits acquired before
  `start` and released in `on_child_finished` or `cancel_all`, avoids a manual length check that can
  drift. The queue must stay explicit to keep FIFO order and to report `queued` status, so the
  semaphore replaces only the counting. For unlimited, skip the semaphore.
- **Record an ADR.** Concurrent child execution changes a core runtime model. Consider a short ADR in
  `docs/architecture-design-records/` once the design is confirmed. Ask the user before creating it.

## Consistency Issues

- [low] Default `4` lets up to four children plus the root issue provider requests at once. Provider
  HTTP concurrency is capped at `MAX_CONCURRENT_PROVIDER_REQUESTS = 4` per connection, so children
  beyond that wait on the provider limiter. This is not a contradiction, but the default is effectively
  bounded by the provider limiter. Document it in `docs/configuration.md` (step 11).
- [low] Concurrent children that write the same file have no coordination. Accepted per the design
  decision. Documented in step 11.

## Implementation Hints

- Scheduler: `src/agent/child_agent_manager.rs`. Module doc at lines 15-21 states the one-child rule.
  `outstanding_count` doc at lines 344-348 already anticipates several active children.
- Spawn path: `src/agent/tool_queue/spawn.rs:27-138`, calling `ChildAgentManager::spawn` at line 72.
  Auto-approved spawns are routed at `src/agent/tool_queue.rs:385-403`.
- Root loop `select!`: `src/agent/runtime.rs:375-427`. Finish arm at line 404. Subagent UI arm at
  line 411.
- Completion handling: `src/agent/runtime/subagents.rs:18-257`. Re-prompt on `outstanding_count == 0`:
  lines 211-240 (immediate) and 241-243 (deferred via `pending_child_finish_reprompt`, `runtime.rs:55`).
- Channels: `src/agent_factory.rs` (`build_child_agent_manager`). Shared `finish` and `subagent_ui`
  are unbounded. Each child's private finish channel is capacity 1.
- Child task: `drive_child_to_completion` at `src/agent_factory.rs:961-1062`. `forward_task` at lines
  970-976. Transcript write at line 978. Child is `tokio::spawn`ed at line 668.
- Child spawn closure type: `SpawnFn`, a `Box<dyn Fn(...) + Send + Sync>` at
  `child_agent_manager.rs:206-208`.
- Lifecycle sends: `send_lifecycle_update` in `child_agent_manager.rs`. Synchronous, on an
  unbounded channel.
- Cancellation: per-child token created at `child_agent_manager.rs:462`. `cancel_all` used by `Stop`
  (`commands.rs:376-440`) and `ClearContext` (`commands.rs:582-590`).
- `session_switch_frozen` / `session_switch_quiescent`: `runtime.rs:70-86`. Both consult
  `outstanding_count() == 0`, which already generalizes.
- Protocol: `src/agent/protocol.rs:418-478` (child variants), `323-334` (`run_id` on pending variants).
- Driver/presenter mapping: `src/tui_driver/updates.rs:150-182`; `src/presenter/domain.rs:86-166`.
- TUI state: `child_conversations: HashMap<Uuid, ChildConversation>` at `src/tui_reducer_state.rs:325`
  is already per run. `running_child` at line 343 is the single slot.
- Approval focus: `Focus::ToolApproval` at `src/tui_reducer_state/events.rs:96`.
  `pending_approval` at `src/tui_render_state.rs:232` is `Option`, one approval for display.
- Config pattern for a numeric setting: `input_history_limit` at `src/config/profile.rs:70-76`,
  default function at `:389-391`, deserializer at `:393`. Validation pattern for a positive `usize`:
  `validate_agent_max_tool_iterations` at `:272-285` (not used here, since `0` is valid).
- Existing tests asserting one-at-a-time behavior: `child_agent_manager.rs:677-747` (unit),
  `tests/tools_agent_loop/subagents.rs:18` and `:333` (integration).
- Test helpers: `tests/common.rs` (`run_ylva_with_provider` line 38, `collect_until` line 111,
  `test_engine_config` line 134), `tests/common/mock_llm_server.rs` (`MockLlmServer` line 241,
  `tool_call_frame` line 61). Log capture: `test_support::capture_logs`.
- Known test quirks: subagent section of `docs/known-test-issues.md` (around line 182) and subscriber
  lock (around line 67).
- Crate features: tokio `full`, `tokio-util` for `CancellationToken`.
