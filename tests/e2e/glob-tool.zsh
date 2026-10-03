#!/bin/zsh
YLVA=${CARGO_TARGET_DIR:-./target}/debug/ylva
ROOT_DIR=$(git rev-parse --show-toplevel)
LOG_DIR=${ROOT_DIR}/logs
LOG_FORMAT=text
PREFIX=$(basename $0 .zsh)

cd ${ROOT_DIR}

make build-debug

test -d "${LOG_DIR}" || mkdir -p "${LOG_DIR}"
echo "**" > "${LOG_DIR}/.gitignore"

function ylva() {
    PREFIX_TIMESTAMPED="${LOG_DIR}/${PREFIX}_$(date +%Y%m%d%H%M%S)"
    LOG_PATH="${PREFIX_TIMESTAMPED}.log"
    OUT_PATH="${PREFIX_TIMESTAMPED}.txt"
    export YLVA_REQUEST_DUMP_PATH=${PREFIX_TIMESTAMPED}.json
    export YLVA_TOKEN_USAGE_DUMP_PATH=${LOG_DIR}/token_stats.jsonl
    ${YLVA} --no-tui --color=yes \
            --log-file ${LOG_PATH} \
            --log-level TRACE --log-format ${LOG_FORMAT} \
            | tee ${OUT_PATH}
}

function seperator() {
    echo
    echo "----------------------------------------------------------"
    echo
}

ylva << '__EOT__'
Search for markdown files in the docs directory (non-recursive).
Report one found path per line!
__EOT__

seperator

ylva << '__EOT__'
Search for markdown files in the docs directory (recursive).
Report one found path per line!
__EOT__

seperator

ylva << '__EOT__'
Search for files in the workspace root (non-recursive).
Count the number of found files!
__EOT__

seperator

ylva << '__EOT__'
Search for files/directories in the workspace root (recursive).
Clamp on 50 matched paths.
__EOT__

seperator

ylva << '__EOT__'
How many files are in project ( incl. git ignored files and hidden files) ?
__EOT__

seperator

ylva << '__EOT__'
How many files are in project ( excl. git ignored files and hidden files ) ?
__EOT__

seperator

ylva << '__EOT__'
How many directories are in project ( incl. git ignored dirs and hidden dirs ) ?
__EOT__

seperator

ylva << '__EOT__'
How many directories are in project ( excl. git ignored dirs and hidden dirs ) ?
__EOT__

seperator

ylva << '__EOT__'
How many files/directories are in project ( incl. git ignored paths and hidden paths ) ?
__EOT__

seperator

ylva << '__EOT__'
How many files/directories are in project ( excl. git ignored paths and hidden paths ) ?
__EOT__
