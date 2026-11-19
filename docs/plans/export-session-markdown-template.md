# Session export Markdown pseudotemplate

This specifies renderer behavior; it is not a configurable runtime template.
`{{name}}` denotes a value defined in the
[rendering rules](export-session-plan.md#rendering-rules). `[[...]]` denotes a
condition or repetition and is not emitted. Other text is generated literally.
The outer fences are documentation delimiters; fence lengths inside them are
illustrative.

`{{subagent_reference}}` is the numbered subagent label, optionally linked to
its bundled transcript. `{{parent_reference}}` and `{{parent_references}}` are
the bundled parent links described in the rendering rules, including the
explicit start fragment when one is available.

````text
# {{heading}}

[[if one bundled parent]]
Parent: {{parent_reference}}
[[end]]

[[if multiple bundled parents]]
Parents: {{parent_references}}
[[end]]

[[for each displayable event, in source order]]
[[if explicit user, assistant, or system message]]
## {{role}}

{{message}}

[[if assistant is interrupted]]
> Assistant response interrupted.
[[end]]

[[for each call in an assistant message]]
### Tool: {{tool_name}}{{call_label}}

**Arguments:**

{{arguments}}

**Result:**

{{result_block}}
[[end]]
[[end]]

[[if implicit harness message]]
> Internal harness notice omitted.
[[end]]

[[if subagent started]]
[[if this is the first rendered start for this child in this parent]]
<a name="{{start_fragment}}"></a>

[[end]]
## {{subagent_reference}} started

Task: {{subagent_task}}
[[end]]

[[if subagent completed]]
## {{subagent_reference}} completed

- Status: {{status_code}}
[[if result is present]]
- Result: {{subagent_result}}
[[end]]
[[end]]

[[if unpaired tool result]]
## Tool result: {{tool_name}}

{{result_block}}
[[end]]

[[if an unknown event can be safely displayed]]
## Other event

```json
{{event_json}}
```
[[end]]

[[if an unsafe or malformed complete event is withheld]]
> Content withheld.
[[end]]

[[if an incomplete trailing line is omitted]]
> Incomplete trailing content omitted.
[[end]]
[[end]]
````

## Parent reference variants

`{{parent_label}}` is `Session` or a numbered subagent label;
`{{parent_path}}` is that parent's bundle-local Markdown filename. The
`{{start_fragment}}` is `ylva-start:subagent-<number>` with the bundle-local
reference number. It is emitted before the first rendered start in that
parent, not derived from the heading text.

```text
[[if the parent has a rendered start heading for this child]]
[{{parent_label}}]({{parent_path}}#{{start_fragment}})
[[else]]
[{{parent_label}}]({{parent_path}})
[[end]]
```

## Argument variants

Render `{{arguments}}` recursively using the rules in the plan. The
`{{argument_key}}` is the escaped key; `{{scalar_value}}` is a scalar or one
of the specified inline-code markers for null and empty values.
`{{markdown_value}}` and `{{fenced_value}}` are argument values, and
`{{fence_language}}` is the literal `bash` or `text` selected by the rules.
Values are sanitized unless `--raw` is used. `{{nested_arguments}}` expands
to the recursively rendered value. Indent every continuation line under its
enclosing list item.

For each object field, choose exactly one alternative in this order:

````text
[[if encoded write content is omitted]]
- **{{argument_key}}:** \[Encoded content omitted.\]
[[else if the value is null or empty]]
- **{{argument_key}}:** {{scalar_value}}
[[else if the value is authored Markdown]]
- **{{argument_key}}:**

  {{markdown_value}}
[[else if the value is fenced content]]
- **{{argument_key}}:**

  ```{{fence_language}}
  {{fenced_value}}
  ```
[[else if the value is a nonempty object or array]]
- **{{argument_key}}:**

  {{nested_arguments}}
[[else]]
- **{{argument_key}}:** {{scalar_value}}
[[end]]
````

For each nonempty array, `{{array_index}}` is its 1-based position. Indent
continuation and nested lines to the content column after the list marker:

```text
[[if the element is a scalar or an empty value]]
{{array_index}}. {{scalar_value}}
[[end]]

[[if the element is a nonempty object or array]]
{{array_index}}.
   {{nested_arguments}}
[[end]]
```

At the root, render a nonempty object or array directly as its list; render a
scalar or empty value directly without a field label.

## Result variants

Render `{{result_block}}` as exactly one of these alternatives:

````text
[[if ordinary tool result]]
```text
{{tool_result}}
```
[[end]]

[[if tool error]]
> Tool error:

```text
{{tool_error}}
```
[[end]]

[[if result is not recorded]]
> Result not recorded.
[[end]]

[[if a result could not be paired unambiguously]]
> Result not paired.
[[end]]

[[if the identified result was withheld]]
> Content withheld.
[[end]]

[[if result is media, even with --raw]]
> Media omitted: {{media_type}}.
[[end]]

[[if a non-media Base64 read result is omitted in sanitized output]]
> Encoded content omitted.
[[end]]

[[if recognized subagent admission]]
{{subagent_reference}} accepted ({{status_code}}).
[[end]]
````
