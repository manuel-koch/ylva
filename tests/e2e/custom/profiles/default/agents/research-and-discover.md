+++
description = "An agent that can perform read-only discovery tasks in workspace using a local LLM."
engine = "local-discovery"
allowed_tools = ["bash", "read", "search"]
[tools.bash]
rules = [
    { command=["grep", "**"], verdict="auto_approved" },
    { command=["ls", "**"], verdict="auto_approved" }
]
+++

You research and discover detailed information on the current topic.
