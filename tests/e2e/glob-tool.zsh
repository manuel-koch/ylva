#!/bin/zsh
YLVA=${CARGO_TARGET_DIR:-./target}/debug/ylva
LOG_DIR=$(git rev-parse --show-toplevel)/logs
LOG_PREFIX=$(basename $0 .zsh)
LOG_FORMAT=json

make build-debug

test -d "${LOG_DIR}" || mkdir -p "${LOG_DIR}"
echo "**" > "${LOG_DIR}/.gitignore"

function ylva() {
  ${YLVA} --log-file $(git rev-parse --show-toplevel)/logs/${LOG_PREFIX}_$(date +%Y%m%d%H%M%S).log \
          --log-level DEBUG --log-format ${LOG_FORMAT}
}

echo "----------------------------------------------------------"
ylva << '__EOT__'
Search for markdown files in the docs directory (non-recursive).
Report one found path per line!
__EOT__

echo "----------------------------------------------------------"
ylva << '__EOT__'
Search for markdown files in the docs directory (recursive).
Report one found path per line!
__EOT__
