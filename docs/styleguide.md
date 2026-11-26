# Styleguide

Authoritative UX/UI rules for the TUI and, where stated, the CLI. New or changed
dialogs, pages and widgets follow this guide so that equal things look and behave equally.

If a change needs a deviation, update this guide in the same commit and say why.

Colors are named functions in `src/theme.rs` (resolved per theme at draw time).

Never hard-code a color or cache one across a theme change. The only exemption is the colors
of syntax-highlighted code, which come from the highlighter theme.

## Colors

This table lists the colors that carry UX/UI meaning in dialogs and pages; it is not exhaustive.
`src/theme.rs` defines every color and is authoritative for the values.

| Meaning | Theme function | Use |
| --- | --- | --- |
| Primary text | `content_color` | Text the user acts on; the focused row |
| Secondary text | `muted_text_color` | Hints, legends, explanations, unfocused rows of a form page, footnotes |
| Heading | `popup_heading_color` | Section headings inside dialogs (bold) |
| Problem or warning | `warning_color` | Validation errors, failures, anything the user must fix or know |
| Tool name | `tool_bubble_foreground_color` | The name of a tool, wherever it is shown as text |
| Tool bubble frame | `tool_bubble_border_color` | Borders of tool bubbles |
| Dialog frame | `popup_border_color` | Border of dialogs |
| Positive/negative buttons | `approve_background_color`, `deny_background_color` | Confirm and Deny buttons |
| Navigation buttons | `navigation_background_color`, `navigation_hover_background_color` | Buttons that only move to another view (`[ ← Back ]`, `[ ⇢ Go to agent ]`) |
| Chat error line | `error_color` | Errors that are part of the conversation transcript |

Rules:

- Color never carries meaning alone; a glyph or wording repeats it.
- Problems inside dialogs and pickers use `warning_color`, not `error_color`.
- In dialogs and pickers secondary text is muted, not dimmed or underlined. Transcript elements
  (thinking and tool bubbles, chat) may use `Modifier::DIM`, and tool bubbles underline their
  section labels (`Arguments:`, `Result:`, `User answered:`). Underline marks labels only, never
  values or interactive text.
- The clarify banner is a transcript element, not a dialog: it may use `Modifier::DIM`.

## Glyphs

| Glyph | Meaning |
| --- | --- |
| `▸` | Focused or selected row in a list, form page or dialog |
| `◂` `▸` around a value | The value cycles with `←`/`→` (`SMALLER_VALUE_GLYPH`, `GREATER_VALUE_GLYPH`) |
| `✎` | The text next to it is editable (`enter` or double-click starts editing) |
| `✗` | A problem line or hint (validation error, failure, warning, rejected action), in `warning_color` or, in the transcript, `error_color`; also the label glyph of a negative action button (`[ ✗ Cancel ]`) |
| `•` | A note (muted) |
| ` · ` | Separator between entries on one line (footer hints and status, e.g. `cost · usage`) |
| `✓` | Confirming action label (`[ ✓ Grant ]`, `[ ✓ Apply ]`) |
| `⟳` | Reload action label that refreshes from external state (`[ ⟳ Reload ]`) |
| `←` | Back or leaving action label (`[ ← Back ]`) |
| `⇢` | Action label that moves to another conversation (`[ ⇢ Go to agent ]`) |
| `●` | The current or active item, in `active_model_color` (the viewed agent in the agents popup) |

## Focus and highlight

- Every slash overlay (command suggestions, `/session`, `/models` list, parameter popup and save
  page), the agents popup and every dialog highlight the focused row by foreground only: bold
  `content_color`, unfocused rows in their normal or `muted_text_color` text. They never use a
  background or reverse video for the focus highlight of a row. Hover feedback of buttons and
  value-cycle glyphs is a separate state and does use a background.
- Exactly one element has the focus. The focused row carries the marker glyph.
- Hover and focus are two separate states, and the most recent input wins: a pointer move onto a
  widget shows its hover style and a `tab` / `shift-tab` moves the focus and clears the hover, even
  while the pointer rests unmoved over the previous widget. Hover shows again only after the
  pointer moves. A hovered button therefore loses its highlight when `tab` moves the focus away.
- A control that is disabled stays visible and says why next to it or in the notes.

## Keyboard

| Key | Behavior |
| --- | --- |
| `↑` `↓` | Move over the rows only; never reach the buttons; no wrapping |
| `tab` / `shift-tab` | Cycle first row → primary button → further buttons when offered → secondary button |
| `←` `→` | Cycle the value of the focused row (`◂ value ▸`), wrapping |
| `space` | Toggle or cycle the focused row's value |
| `enter` | On a row: start editing its text, or activate it; on a button: activate it |
| `esc` | Leave the innermost layer; while editing, restore the previous value |
| `ctrl-c` | Same as `esc` in dialogs; never quits from inside one |
| `pgup` `pgdn` `home` `end`, mouse wheel | Scroll content that does not fit |

Rules:

- No single-letter shortcut for something a value cycle or `enter` already does; shortcuts are
  not discoverable. Every action is reachable with the keys above.
- While text is edited, typing, `←` `→` `home` `end` (move the caret), `delete`, `backspace`,
  `ctrl-u` (clear), `enter` (keep) and `esc` (restore) apply; `↑` `↓` `tab` and the other
  navigation keys are inactive. `←` `→` move the caret, never cycle a value.
- Mouse: a click on a row focuses it and, for cyclable values, cycles it; a double-click on text
  marked with `✎` starts editing it; a click on a button activates it. Everything on the keyboard is
  reachable with the pointer and vice versa.
- Exception: the approval and clarify prompts. There `esc` only removes button focus (clarify also
  clears the input) and never decides; `ctrl-c` denies the tool or aborts the clarify. A stray `esc`
  must not approve, deny or abort pending work.
- Every key is listed in [keybindings](./keybindings.md) in the same commit.

## Dialogs and pages

- Dialogs are framed with a full single-line border in `popup_border_color` and
  titled with a short noun phrase padded by one space (` Grant similar calls `). The grant
  dialog, like the slash-command panel, spans the input's width and its bottom edge sits
  directly above it; only the space above the input is available to it.
- Padding: one blank column between the border and the content on the left and on the right; the
  right one carries the scroll indicator. No blank rows inside the frame except between sections
  and items.
- Sections use a bold heading in `popup_heading_color`, then indented muted or primary text.
  Typical order: items, explanation of choices, notes, buttons.
- Rows of editable items are separated by one blank line. Multi-line values hang under the first
  line, indented past the marker.
- Long text is word-wrapped to the dialog width, never cut; wrap to the dialog, not the terminal.
- Avoid resizing a dialog while the user works, because it flickers. Prefer to reserve the room
  that a choice or editing hint adds (see the grant dialog) so the layout stays still; resize
  only when reserving would waste a lot of space or the content itself changes.
- Content that does not fit scrolls (see [Scroll indicators](#scroll-indicators)); the buttons
  stay pinned below it.
- Too small a terminal shows a one-line hint in footer instead of a broken dialog; `esc` still
  works.
- Hints appear where they apply and only while they apply (for example the wildcard hint while a
  `bash` rule is edited), muted, one sentence.
- Pages (such as the `/models` save page) follow the same rules inside the picker's frame: the
  focus uses the foreground-only highlight and the buttons sit in the last rows.

## Buttons

- Written ` [ label ] `, with a leading `✓` for the confirming action, `⟳` for the reload action
  (`[ ⟳ Reload ]`, the grant dialog) and `←` for going back
  (`[ ← Back ]`); the label is a verb or a short verb phrase.
- Normal: `approve_background_color` as foreground; hovered: `button_text_color` on the hover
  background; focused: `button_text_color` on the normal background, bold (`button_style`).
- Primary action first, secondary after it, separated by one space; buttons share a row and wrap
  to the next row on narrow dialogs rather than being cut.
- A disabled button is muted and not clickable; the reason is visible in the notes or problem
  lines.
- `tab` reaches buttons, `enter` or a click activates them, `esc` is the back action.
- A button that denies or discards something with consequences (such as cancelling a question,
  which gives no answer, or deleting a session) is labelled with `✗` and uses
  `deny_background_color`; `←` is only for leaving without consequence.
- A button that only navigates (back to the previous view, to another conversation) is neither
  confirm nor discard: it uses `navigation_background_color` and
  `navigation_hover_background_color` instead of the approve or deny colors, with the same hover and
  focus styles.
- Roles by color: approve = confirm, deny = discard or abort, navigation = move.

## Multi-value cycle input

- Shown as `◂ value ▸`; the value is a short lowercase noun and is padded to the longest value so
  neighbouring text does not move when it changes.
- `←` `→` and `space` on the focused row cycle it, wrapping; a click on the row or on a glyph
  steps it (the glyph steps in its direction). Hovering a glyph highlights it.
- The current value is always visible; all values are explained in the dialog (for example
  *Scope*) when their meaning is not obvious.
- The value does not change the row's color, so an unfocused row keeps the same look.
- A single-valued row shows the plain value without glyphs.

## Single-line text input

- Marked by `✎` before the text; the text is not underlined or boxed.
- Editing starts with `enter` or a double-click on the text; typing inserts at the caret, `←` `→`
  `home` `end` move it, `backspace` and `delete` remove, `ctrl-u` clears, `enter` keeps, `esc`
  restores the previous value. Pages with a permanent field (the engine name) edit as soon as it has
  the focus.
- The caret is the terminal's own cursor placed at the insertion point; no glyph is inserted, so
  the text never shifts. While a value is not edited the cursor is hidden.
- The input is validated live; an invalid value gets a `✗` problem line and blocks the confirming
  button. Show a hint (muted, one sentence) while editing when the syntax is not obvious.
- Text wider than the dialog wraps (dialogs) or is truncated with `…` (pickers); it is never cut
  mid-glyph.

## Scroll indicators

- Content taller than its area scrolls, and a scrollbar occupies the rightmost content column:
  arrows at both ends and a thumb in `chat_scrollbar_color` (the track is dimmed).
- The thumb size and position follow the share of visible content; no scrollbar is drawn when
  everything fits.
- Wheel, `pgup`, `pgdn`, `home` and `end` scroll.
- Pinned parts (buttons, headers) never scroll away.

## Toasts, footer and selection

- A toast is a one-line framed message at the bottom right in `toast_fg_color` on
  `toast_bg_color`; this background is the exception to the foreground-only rule. A confirmation
  starts with `✓`, a rejected action with `✗` (`✗ Paste rejected: …`).
- The footer shows keybinding hints, separated by ` · `. An alert hint (such as a terminal that
  is too small for a dialog) starts with `✗` and is bold in `deny_background_color`.
- Text selection (chat, clarify banner) is drawn with a background (`chat_selection_bg`, the
  banner's own selection color); selected text keeps its foreground. Selection is not a focus
  highlight.
- Every error or warning hint, in dialogs, the transcript, the footer and toasts, carries `✗`
  and states what is wrong and what to do.

## Wording

- Labels are short lowercase nouns for values (e.g. `one-time`, `temporary`, `persistent`) and
  sentence-case phrases for explanations, ending in a period when they are sentences.
- Count lines name what is counted (`2 more commands are already allowed …`).
- A problem line states what is wrong and what to do, in one sentence.
- The same concept keeps the same word in the dialog, the docs and the code
  (see [CONTEXT](../CONTEXT.md)).

## Recovery

- A failed action keeps the dialog open, shows the reason as a `✗` problem line and offers the
  next step; nothing the user typed is lost.
- Pending work (such as a tool approval) stays pending until the user decides.

## CLI

The CLI follows the TUI's vocabulary and problem style:

- Use the same words for the same concepts (grant scopes, engine, profile).
- A problem states what is wrong and what to do, in one sentence.
- Never rely on color alone, and keep output readable when piped.
- Problem lines on stderr start with `error:` or `warning:` instead of `✗`, so they stay greppable
  when piped.
