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
Use search tool to find markdown files that contain the
case-insensitive word `tool`.
__EOT__

echo "----------------------------------------------------------"
ylva << '__EOT__'
Use search tool to find Rust source files that contain the
case-sensitive word `SearchTool`, provide 4 lines of context for each match.
__EOT__

echo "----------------------------------------------------------"
ylva << '__EOT__'
Use search tool to find Rust source files that contain the
case-sensitive word `BashTool`.
Report only the paths that matched.
__EOT__

echo "----------------------------------------------------------"
ylva << '__EOT__'
Use search tool to find Rust source files that contain the
case-sensitive regexp `\w+Tool`.
Report only the matched regexp without path/line info.
__EOT__
