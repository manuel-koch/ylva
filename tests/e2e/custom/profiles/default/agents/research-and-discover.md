+++
description = "An agent that can perform read-only discovery tasks in workspace solely using builtin tools and a local AI-Model."
engine = "local-discovery"
[tools.bash]
rules = [
    { command=["grep", "**"], verdict="auto_approved" }
]
+++

You research and discover detailed information on the current topic.
