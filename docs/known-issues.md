# Known Issues

Workarounds and explanations for persistent test-environment quirks.

## Tracing subscriber isolation in tests

### Issue

Concurrent `tracing` subscriber registration corrupts per-callsite interest
caches, causing log events to be silently dropped in test runs.

`with_default(subscriber, ...)` calls `Dispatch::new(subscriber)` internally,
which adds the dispatcher to a process-global `LOCKED_DISPATCHERS` list and
then rebuilds every callsite's interest by ANDing together all live dispatchers
(`Interest::and`). When two tests run `with_default` concurrently, an
INFO-only dispatcher from one test can AND with the DEBUG dispatcher from
`capture_logs`, yielding `Interest::never()` for DEBUG callsites — causing
those events to be silently dropped even though the capturing subscriber is
interested. The same applies to `init_logging`, which calls
`set_global_default` → `rebuild_interest_cache` and can reset callsite
interests globally.

A subtler variant: a callsite is only registered on its **first** access.
`capture_logs` calls `rebuild_interest_cache()` before running the body, which
updates all *already-registered* callsites. But if a concurrent test (without
the lock) first accesses a callsite from a thread with no subscriber, the
callsite is registered with `Dispatch::none()` → `Interest::never()`. Later,
when `capture_logs`'s body hits that callsite, the cached "never" causes the
event to be dropped — `rebuild_interest_cache()` never saw it because it
wasn't registered yet when that was called.

### Workaround

Any unit test that calls `tracing::subscriber::with_default(...)` or
`init_logging(...)` **must** acquire `SUBSCRIBER_LOCK` for the entire duration:

```rust
let _lock = crate::test_support::SUBSCRIBER_LOCK
    .lock()
    .expect("subscriber mutex poisoned");
```

Any test that calls a function exercised by `capture_logs` tests (such as
`read_file`, `write_file`, or `glob_files`) **must** use `run_locked` instead
of `#[tokio::test]`:

```rust
#[test]
fn my_test() {
    crate::test_support::run_locked(async {
        // async test body
    });
}
```

Holding `SUBSCRIBER_LOCK` for the full duration of the `with_default` closure
(and the surrounding `Dispatch` lifetime) serialises subscriber registration
across tests and prevents this class of flaky failure.

## Process-global cwd mutation in tests

### Issue

`init_logging_relative_filename_uses_current_directory` temporarily changes the
process-global current working directory (`std::env::set_current_dir`) while
holding `SUBSCRIBER_LOCK`. Any test that reads `std::env::current_dir()` without
holding the lock can race with this mutation and observe an unexpected cwd,
causing spurious assertion failures.

### Workaround

Any test that reads or depends on the current working directory **must** also
acquire `SUBSCRIBER_LOCK` for the duration of the read:

```rust
let _lock = crate::test_support::SUBSCRIBER_LOCK
    .lock()
    .expect("subscriber mutex poisoned");
let cwd = std::env::current_dir().expect("cwd");
```

## `cfg(test)` does not reach library code from integration tests

### Issue

The Rust compiler sets `cfg(test)` only for the crate currently being compiled
as a test harness. For integration tests (`tests/*.rs`) the `ylva` library is
compiled as a normal dependency — **`cfg(test)` is NOT set** for library source
files when the caller is an integration test.

Practical consequence: `#[cfg(not(test))]` guards inside `src/` do **not**
prevent execution when called from `tests/*.rs`. Any side-effect that must be
suppressed in tests (OS clipboard writes, network calls, file I/O, …) must be
abstracted via a trait or closure injected by the caller, so integration tests
can pass a no-op implementation while unit tests continue using `#[cfg(test)]`
guards for their own call sites.

### Workaround

Abstract the side-effectful operation behind a trait or callback parameter and
inject a no-op implementation from integration test call sites. Do not rely on
`#[cfg(not(test))]` in library code to suppress behaviour from integration
tests.

## Backlog of issues discovered while testing

- [x] "Phase 6.12 Child-Agent Persistence" seems misplaced. Its topic is the
      persistence of subagent conversation. But currently we have no persitence
      at all. The plan doesn't explain why we need this ( without main persistence
      in the first place ).

- [x] "Phase 6.13 — Headless" seem obsolete now, AFAIK we already have a CLI flag to
      enable subagents.

- [x] When a tool approval arrives for a tool that was called from a subagent,
      the approval dialog doesn't indicate the "agent" scope. User doesn't
      see whether the tool wants to start in main-conversion or subagent.
      Maybe we can only address this in a later phase when drill-down
      on subagent conversion-list is implemented ?

- [x] Just noticed that the TUI shows "Running Tool.." in subagent conversation
      list while the subagent is streaming/thinking and actually not running a tool.
      Maybe we can remove the placeholder hint in the input-field and just
      update the footer to indicate that subagent is working/thinking ?

- [x] Tool bubble in subagent-conversation-list can't be expanded/collapsed on
      click on title or the expand/collapse indicator.
      It works in the main conversation-list though.

- [x] Noticable lag when clicking the subagent bubble expand/collapse, due to
      double-click detection.
      Can we reduce the timeframe needed for a double-click, to reduce that lag ?

- [x] While subagent conversation-list is open, the user-message draft input-field
      shows "Subagent is working..." unconditionally, even when user starts typing some text into the input field.
      Whats the purpose of that hint ? I can see that the input-field is still
      a useful think in the subagent conversation-list, although user can't type any
      steering message.
      But user may call a slash command there too - future work though.

- [x] While looking at the subagent conversation-list, the subagent becomes idle and
      is done. But still the footer shows "Streaming..." likely because the parent
      conversation is working on the result of the subagent.
      Shouldn't the footer indicate that the subagent is done and that parent
      is still busy ?

- [x] The keybinding hint for "ctrl-b: back" should maybe be rephrased, it's
      not clear what "back" means.
      Maybe ctrl-b is the wrong keybinding for "back" ?
      What about esc ?
      - input field has text -> clear input field
      - input field is empty -> esc in subagent conversation-list should not
      "cancel" the conversation, rather go back to parent and there user can
      abort/steer the main conversation.
      Fixed: `ctrl-b` dropped entirely. Drilling into a subagent's view is
      now only via the existing double-click gesture; `esc` with an empty
      draft in the drill-in view navigates back to the root conversation
      instead of acting on the root agent's busy/idle state (a non-empty
      draft still just clears, same as everywhere else).

- [x] The "enter: send" footer hint in the subagent drill-in view is
      misleading — without any slash-command implementation, the user can't
      do anything to the subagent from its own view (no steer, no cancel).
      Turned out `Enter` with a non-empty draft wasn't a no-op there either:
      it silently queued a steer for the *root* agent while the user was
      looking at the child's transcript, with no visual indication of which
      conversation the message actually went to.
      Fixed: `Enter` is now a no-op while viewing a running subagent's
      drill-in transcript (`presenter::handle_key_main`), and the footer
      omits `enter: send` entirely in that view.

- [x] When drilling-in on a subagent conversation-list while it is running,
      the list should scroll to the end of the conversation.

- [x] When doing a ctrl-b to jump back the parent agent conversation-list,
      should the user land back at the end of current conversation-list
      or instead at the subagent bubble he just returned from ?
      IMO the latter is more intuitive when user just drilled-in from the list
      and ctrl-b will him bring back to where he drilled-in.
      Fixed: `switch_to_child_view` now saves the root's scroll position into
      `AppState.saved_root_scroll` before pinning to the bottom; manual
      `switch_to_root_view` (Ctrl+B) restores it.

- [x] When agent spawns (multiple) subagents, scrolling up to see the first
      subagent bubble in main agent conversation-list.
      As soon as there is an update (response, not thinking) in main agent,
      the conversation-list scrolls down to the end - it should stay where
      user just scrolled to.
      This affects going back from a subagent conversation-list to the main
      conversation-list too, instead of landing on the subagent where user
      drilled-in, the user lands on the final response of llm in the main
      conversation-list.
      Fixed: `DomainEvent::StreamDone`/`StreamCancelled` handlers in
      `presenter.rs` (5 occurrences across `domain_pending`, `domain_streaming`,
      `domain_aborting`, and the clarify/tool-approval mode handlers)
      unconditionally forced `scroll_locked = true; scroll_offset = 0;` whenever
      a response finished, overriding any manual scroll-up. Removed the forced
      reset so a completed response respects the existing scroll-lock state,
      matching how `StreamChunk`/`StreamThinkingChunk` already behave. This
      was also the root cause of the drill-in/back symptom — the
      `saved_root_scroll` restore (previous entry) was being immediately
      overridden by the next `StreamDone`.

- [x] The scroll index is not preserved fully when returning from a subagent
      conversation to the main conversation while the main conversation got
      more messages while away. The user lands below/after the bubble they
      drilled in from instead of at that very same bubble.
      Fixed: `saved_root_scroll` (a raw absolute row number captured once at
      drill-in time) drifts if the root conversation grows while the child
      view is open, since new messages/tool bubbles shift row indices at or
      below the saved offset. `switch_to_child_view` now also captures the
      drilled-into `spawn_subagent` bubble's own `Uuid` into
      `AppState.saved_root_scroll_bubble`. `switch_to_root_view` moves it into
      `AppState.pending_scroll_to_bubble`, and `render::sync_from_draw`
      resolves it against that frame's freshly-recomputed
      `AppState.bubble_title_rows` (available once `active_view` has flipped
      back to `Root`), scrolling to the bubble's *current* row regardless of
      how much the root conversation grew while away. `saved_root_scroll`'s
      raw-offset restore remains as an immediate one-frame fallback (avoids a
      blank/wrong-looking frame) for the rare case the bubble can no longer
      be found.

- [ ] When llm spawned many subagents and user drills-into a specific subagent
      conversation-list, that list constantly re-renders whenever a new subagent
      starts. Seems like the drill-in just works on the last active subagent
      and not on a particular subagent.
      I noticed that becasue the subagent conversation bubble header keeps rendering
      different run_id and the content of the conversation-list keeps changing
      while viewing it.
      Similar issue when there is an active subagent and a queued one.
      Double click on the active subagents opens a conversation-list
      of the queued subagent, not the one user clicked.

- [x] llm spawned two subagents.
      1st is done, User is looking at its conversation.
      2nd subagent is started in the background, triggers a bash tool approval.
      Now user sees "Subagent tool 'bash' requests your approval:" message in the
      subagent view that renders the "done" subagent, not the active subagent.
      A approval should immediately switch to that subagent view, handle the
      approval, and afterwards return to the subagent the user was viewing before
      the approval request arrived.

- [ ] Subagent just renders "Subagent working..." regardless of whether it is
      streaming or thinking. Can we apply the same for the main conversation-list,
      using "Working..." ?

- [ ] We should split `src/agent/tools/fs.rs` into one module per tool.
      "edit" and "search" are filesystem related tools too, but they already
      have a dedicated module.

## Code style smell

Code fragments like the following seem to be written to avoid "too many args" linter
issue. But it looks like the introduction of the "params" struct is just to
silence the linter warning, the implementation just packs some args into
a struct and immediately unpacks the struct into local vars inside the method.
If this is the way the linter issue should be fixed ( is this actually
best practice ? ), I would rather just suppress the linter issue
instead of writing so much bogus code fragments with no benefit wrt readability.

```rust
/// Grouped parameters for [`draw_approval_footer`] (avoids an excessive
/// positional-argument count; mirrors [`ClarifyBannerParams`]).
struct ApprovalFooterParams<'a> {
    tool_name: &'a str,
    is_subagent: bool,
    input_is_empty: bool,
    mouse_pos: Option<(u16, u16)>,
    hint: Option<&'a str>,
}

/// Returns the button `Rect`s for mouse hit-testing.
fn draw_approval_footer(
    frame: &mut Frame,
    row_1: Rect,
    row_2: Rect,
    params: ApprovalFooterParams<'_>,
) -> Option<ButtonAreas> {
    let ApprovalFooterParams {
        tool_name,
        is_subagent,
        input_is_empty,
        mouse_pos,
        hint,
    } = params;
    .....
```

## Features document to verbose

The `features.md` document contains many code-like references and detailed
implementation details.
This document should be a user-facing documentation of the features of Ylva.
We should condense the documentation and get rid of any in-detail-implementation references and code-snippets.
If this information is of any use, we should mayne move the implemenentation
detail info into a dedicated file instead ?
