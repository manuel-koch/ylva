# Ylva diagrams

Mermaid sources describe selected implemented behavior, not planned features or
exhaustive generated inventories. See
[implementation notes](../implementation-notes.md#diagram-scope) for scope.

| Source | Purpose |
|---|---|
| [class-diagram.mmd](class-diagram.mmd) | Interfaces, runtime ownership, and module relationships |
| [tui-reducer-state-diagram.mmd](tui-reducer-state-diagram.mmd) | Execution mode and independent input focus |
| [turn-execution-sequence.mmd](turn-execution-sequence.mmd) | Requests, tools, approval, and steering |
| [subagent-lifecycle-sequence.mmd](subagent-lifecycle-sequence.mmd) | Admission, FIFO scheduling, interaction, and completion |
| [compaction-data-flow.mmd](compaction-data-flow.mmd) | Transcript preservation and bounded model views |
| [session-lifecycle-state-diagram.mmd](session-lifecycle-state-diagram.mmd) | Root session activation and switching |
| [session-storage-state-diagram.mmd](session-storage-state-diagram.mmd) | Durable session representations and offline maintenance |
| [configuration-policy-flow.mmd](configuration-policy-flow.mmd) | Resource precedence, prompt assembly, and capability policy |
| [tui-event-flow.mmd](tui-event-flow.mmd) | Driver, presenter, effects, state, and rendering |

## Generate SVG

Use the Make targets `render-all-diagrams` for all sources or
`docs/diagrams/<name>.svg` for one source. See
[Diagram rendering](../development-workflow.md#diagram-rendering) for usage,
requirements, and forced regeneration.
