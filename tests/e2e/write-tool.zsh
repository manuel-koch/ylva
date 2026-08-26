#!/bin/zsh
YLVA=${CARGO_TARGET_DIR:-./target}/debug/ylva
ROOT_DIR=$(git rev-parse --show-toplevel)
LOG_DIR=${ROOT_DIR}/logs
LOG_PREFIX=$(basename $0 .zsh)
LOG_FORMAT=text

cd ${ROOT_DIR}

make build-debug

test -d "${LOG_DIR}" || mkdir -p "${LOG_DIR}"
echo "**" > "${LOG_DIR}/.gitignore"

function ylva() {
  ${YLVA} --no-tui \
          --log-file $(git rev-parse --show-toplevel)/logs/${LOG_PREFIX}_$(date +%Y%m%d%H%M%S).log \
          --log-level DEBUG --log-format ${LOG_FORMAT}
}

echo "----------------------------------------------------------"
ylva << '__EOT__'
Create file test.txt with the following content:

```text
Lorem ipsum hello world!
Mary had a little lamb!
And everywhere Mary went,
the little lamb would surely follow her.
```
__EOT__

echo "----------------------------------------------------------"
ylva << '__EOT__'
Create file ../test.txt with the following content:

```text
Lorem ipsum hello world!
```
__EOT__
