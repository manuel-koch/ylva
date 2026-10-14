#!/bin/zsh
YLVA=${CARGO_TARGET_DIR:-./target}/debug/ylva
ROOT_DIR=$(git rev-parse --show-toplevel)
LOG_DIR=${ROOT_DIR}/.ylva/logs
LOG_FORMAT=text
PREFIX=$(basename $0 .zsh)

cd ${ROOT_DIR}

make build-debug

function ylva() {
    PREFIX_TIMESTAMPED="${LOG_DIR}/${PREFIX}_$(date +%Y%m%d%H%M%S)"
    LOG_PATH="${PREFIX_TIMESTAMPED}.log"
    OUT_PATH="${PREFIX_TIMESTAMPED}.txt"
    export YLVA_REQUEST_DUMP_PATH=${PREFIX_TIMESTAMPED}.json
    export YLVA_TOKEN_USAGE_DUMP_PATH=${LOG_DIR}/token_stats.jsonl
    ${YLVA} --no-tui --color=yes \
            --log-file ${LOG_PATH} \
            --log-level DEBUG --log-format ${LOG_FORMAT} \
            | tee ${OUT_PATH}
}

function seperator() {
    echo
    echo "----------------------------------------------------------"
    echo
}

ylva << '__EOT__'
Read lines 31 to 32 of (big file!) tests/e2e/read-tool.zsh and do a spell check.
Repoort only the findiings. Do not chaange anything!
__EOT__

seperator

ylva << '__EOT__'
Read the first 10 characters of tests/e2e/read-tool.zsh.
Report only the plain read text snippet as-is.
__EOT__

seperator

ylva << '__EOT__'
Read the first 32 bytes of tests/e2e/read-tool.zsh.
Report only the plain read data snippet as-is.
__EOT__

seperator

ylva << '__EOT__'
What content does `.rumdl.toml` have ?
__EOT__
