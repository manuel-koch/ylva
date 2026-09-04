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

PREFIX_TIMESTAMPED="${LOG_DIR}/${PREFIX}_$(date +%Y%m%d%H%M%S)"
LOG_PATH="${PREFIX_TIMESTAMPED}.log"
OUT_PATH="${PREFIX_TIMESTAMPED}.txt"
export YLVA_REQUEST_DUMP_PATH=${PREFIX_TIMESTAMPED}.json
${YLVA} --log-file ${LOG_PATH} \
        --log-level TRACE --log-format ${LOG_FORMAT} \
        --auto-approve-spawn
