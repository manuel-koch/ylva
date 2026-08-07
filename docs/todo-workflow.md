# Todo Task Workflow

New tasks are added as needed.

## Todo Task Execution

When the agent finishes implementation work on a task, the agent marks that
task `[x]` in `docs/todo.md` before reporting completion.

Checkmarked (`[x]`) tasks are a stable resting state — they remain in `docs/todo.md`
as "to-be-merged-next" remainders until the user explicitly triggers the
cleanup-and-merge operation (see "## Cleanup Todo and merge to Features" below).
The agent must not merge them, remove them, or treat them as candidates for cleanup
during normal task execution.

Follow the user's instructions precisely!
Do not add extra steps, merge documents, or perform cleanup actions unless explicitly asked!
If you are unsure what the user wants, ask back using `clarify` with specific options
or an open-ended question — do not guess or assume unstated intent.

## Cleanup Todo and merge to Features

Only perform cleanup of todo document when user requests it explicitly !
This means the user says "clean up the todo" or "merge done tasks".
Implementing tasks or marking them `[x]` does NOT trigger this merge-operation !

When it does apply, follow these steps.

### Cleanup and merge steps

Force re-reading related documents, `features.md` and `todo.md` document to fully
grasp their current content !

- every document in `./` directory (this directory)
- [README](../README.md)
- [AGENTS](../AGENTS.md)

Check the documentation files for consistency and align them to `features.md`.

Merge finished todo tasks with the features document:

- Check if there is an existing feature that matches task content fully/almost/partly
  - If feature is matched fully, just remove the task from todo
  - If feature is matched partly/almost, check what the diff to task content is and decide if feature
    text should be updated or a new distinct feature be introduced with the task content
  - If feature is not matched, introduce a new distinct feature with tasks content.
    If needed check if the new feature belongs to a new section/heading within the documents
    to group features by topics.
  - if in doubt if a task matches a feature, ask the user how to proceed, provide proposal
    what you think would fit best.
- Don't add explicit features that would stem from task that have subject of
  tests / refactoring / housekeeping or fixing bugs
- Remove finished task from todo when merged with feature document
- Don't remove empty todo sections - we might add new tasks to it,
  add a placeholder "- [ ]" task if necessary.

For updated features document, consult the sources/implementation to check if features
are actually implemented the way they are currently stated in the feature description.
Update the feature descriptions to match the current implementation.
