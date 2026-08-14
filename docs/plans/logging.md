# Logging in Ylva

We should establish a logging infrastructure for the CLI and TUI code
to be able to log messages of various severities.

All logs should be saved in a runtime selectable file.

Log format includes

- timestamp
- severity
- source-file & line, (module?)
- message

Structured logging with JSONL could be an option.

Would need new commandline options to select

- path to save log to
- severity ( DBG, INFO, WARN, ERR or similar ) for logs that
  are stored in the log
- selecting a severity, results in all logs of this level or greater
  to be saved in the log.
  - INFO: just INFO, WARN and ERR are logged
  - DBG: DBG, INFO, WARN and ERR are logged
