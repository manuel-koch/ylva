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
    cat << '__POEM__' > "${LOG_DIR}/poem.md"
A little lamp, so small and bright,
Dispelling shadows of the night.
A gentle glow, a steady beam,
Living out a lamp's soft dream.
__POEM__
    PREFIX_TIMESTAMPED="${LOG_DIR}/${PREFIX}_$(date +%Y%m%d%H%M%S)"
    LOG_PATH="${PREFIX_TIMESTAMPED}.log"
    OUT_PATH="${PREFIX_TIMESTAMPED}.txt"
    export YLVA_REQUEST_DUMP_PATH=${PREFIX_TIMESTAMPED}.json
    export YLVA_TOKEN_USAGE_DUMP_PATH=${LOG_DIR}/token_stats.jsonl
    ${YLVA} --no-tui --color=yes \
            --log-file ${LOG_PATH} \
            --log-level DEBUG --log-format ${LOG_FORMAT} \
            --auto-approve-edit \
            | tee ${OUT_PATH}
}

function seperator() {
    echo
    echo "----------------------------------------------------------"
    echo
}

ylva << '__EOT__'
Replace all occurance of "lamp" in `logs/poem.md` by "frog" using the `edit` tool!
__EOT__

seperator

ylva << '__EOT__'
Replace first occurance of "lamp" in `logs/poem.md` by "frog" using the `edit` tool!
__EOT__

seperator

ylva << '__EOT__'
Replace line 2+3 in `logs/poem.md` by one line "hello world" using the `edit` tool!
__EOT__
