+++
description = "An agent that can perform read-only discovery tasks in workspace using a local LLM."
engine = "local-llm"
allowed_tools = ["bash", "read", "search"]
[tools.bash]
rules = [
    { command=["grep", "**"], verdict="auto_approved" },
    { command=["ls", "**"], verdict="auto_approved" }
]
[tools.read]
rules = [
    { rule = "outside_workspace", verdict = "auto_approved" },
    { rule = "dot_path",          verdict = "user_approval_required" },
    { rule = "cross_filesystem",  verdict = "user_approval_required" }
]
+++

You research and discover detailed information on the current topic.
