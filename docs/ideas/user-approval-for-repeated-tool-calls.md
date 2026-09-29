# User approval required for repeated tool calls

I have seen LLM triggering the same tool call ( e.g. search or bash grep )
with the same arguments over and over again.

We should detect such a rapid call of same tools and enforce user-approval
in such a case so that a LLM running wild with non-sense tool calls
can get aborted.

To detect same tool calls, we can use the current conversation history.
Find repeated same calls not just when they are consecutive.
I've seen LLM trigger a group of same tool calls one after another.

Maybe adding a hash to every tool call would make searching for duplicates
easier. A tool call could contain a hash over its name and attributes,
thus finding duplicates is just a matter of comparing hashes.

When the same tool call is done three times within 5sec, then the 3rd tool call
should carry verdict `user-approval-required`, to allow user to abort such a loop
of same tool calls.
