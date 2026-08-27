# Sample prompts

```shell
export OLLAMA_BASE_URL=http://localhost:11434/v1

make format lint test-coverage build-debug-and-run-with-logs
```

```text
Implement open tasks in section "Phase 6.0" of
docs/plans/phase6-terminal-superpowers.md

Don't stage or commit anything in git!
Check linter/tests/coverage for issues!
```

```text
Why is the sky blue?
And why appear oceans green ?
Why are clouds sometimes shiny bright white or dark gray black ?
```

```text
Please explain more.
```

```text

      Please read file `docs/development-workflow.md` and
      create a three sentence abstract of its content.

```

```text
How many markdown files are in `docs` folder ?
```

```text
Why is the sky blue?
And why appear oceans green ?
Why are clouds sometimes shiny bright white or dark gray black ?

Please read file `docs/development-workflow.md` and
create a three sentence abstract of its content.

Tell me a short story (200 words) about a little lamb that went to school.

Please read file first 10 lines of `docs/development-workflow.md`
and summaries it in one sentence.

How many files are in project ?

How many markdown files are in `docs` folder ?

Please read file `docs/development-workflow.md` and
count the words in its content.

Create a four line poem about a little lamp and save it to `logs/poem.md`.

Summarize current conversation and save it to `../logs/summary.md`.

Create a 50 word sentence ( without newline ) and save it too `../logs/long-line.md`.

Run `python3` to find out which version it is and where the python interpreter
is located.

Run `tests/e2e/long-running.bash`.

Run the following command in bash: `date ; sleep 3 ; echo lorem ipsum ; date ; sleep 3 ; echo lorem ipsum ; date ; sleep 3 ; echo lorem ipsum ; date ; sleep 3 ; echo lorem ipsum`

Run the following command exactly like given (as-is, don't re-write/collapse linebreaks ) in bash:
```shell
for n in seq 1 5 ; do
  echo "$(date) lorem ipsum"
  sleep 3
done
```

Run `tests/e2e/heavy-output.bash`.
```
