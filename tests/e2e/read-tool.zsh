#!/bin/zsh
YLVA=${CARGO_TARGET_DIR:-./target}/debug/ylva
LOG_DIR=$(git rev-parse --show-toplevel)/logs
LOG_PREFIX=$(basename $0 .zsh)
LOG_FORMAT=text

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
Read lines 20 to 21 of (big file!) tests/e2e/read-tool.zsh and do a spell check.
Repoort only the findings. Do not change anything!
__EOT__

echo "----------------------------------------------------------"
ylva << '__EOT__'
Read the first 10 characters of tests/e2e/read-tool.zsh.
Report only the plain read text snippet as-is.
__EOT__

echo "----------------------------------------------------------"
ylva << '__EOT__'
Read the first 32 bytes of tests/e2e/read-tool.zsh.
Report only the plain read data snippet as-is.
__EOT__
