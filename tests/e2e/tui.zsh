#!/bin/zsh
YLVA=${CARGO_TARGET_DIR:-./target}/debug/ylva
ROOT_DIR=$(git rev-parse --show-toplevel)
if [ -n "${YLVA_HOME}" ] ; then
    LOG_DIR=${YLVA_HOME}/logs
else
    LOG_DIR=${ROOT_DIR}/logs
fi
LOG_FORMAT=text
PREFIX=$(basename $0 .zsh)

cd ${ROOT_DIR}

make build-debug

test -d "${LOG_DIR}" || mkdir -p "${LOG_DIR}"
echo "**" > "${LOG_DIR}/.gitignore"

PREFIX_TIMESTAMPED="${LOG_DIR}/${PREFIX}_$(date +%Y%m%d%H%M%S)"
LOG_PATH="${PREFIX_TIMESTAMPED}.log"
OUT_PATH="${PREFIX_TIMESTAMPED}.txt"
export YLVA_REQUEST_DUMP_PATH=${PREFIX_TIMESTAMPED}.json
export YLVA_TOKEN_USAGE_DUMP_PATH=${LOG_DIR}/token_stats.jsonl
${YLVA} --log-file ${LOG_PATH} \
        --log-level TRACE --log-format ${LOG_FORMAT} \
        --auto-approve-spawn "$@"
