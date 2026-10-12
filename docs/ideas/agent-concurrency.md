# Agent concurrency

Current implementation runs subagents one after another,
there is just on subagent at most running concurrently with the main
agent.

It should be possible to run some/all subagents concurrently.
We should introduce a customizable concurrency setting to limit
how many subagents can concurrently run with main agent.
