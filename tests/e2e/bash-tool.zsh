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
            $* \
            | tee ${OUT_PATH}
}

function seperator() {
    echo
    echo "----------------------------------------------------------"
    echo
}

ylva << '__EOT__'
Run `python3` to find out which version it is and where the python interpreter
is located.
__EOT__

seperator

ylva --auto-approve-bash << '__EOT__'
Run `python3` to find out which version it is and where the python interpreter
is located. Supply a label for bash tool too.
__EOT__

seperator

ylva << '__EOT__'
Run the following command exactly like given (as-is, don't re-write/collapse linebreaks ) in bash:
```shell
for n in seq 1 5 ; do
  echo "$(date) lorem ipsum"
  sleep 3
done
```
__EOT__

seperator

ylva --auto-approve-bash << '__EOT__'
Run the following command exactly like given (as-is, don't re-write/collapse linebreaks, supply a label for it ) in bash:
```shell
for n in seq 1 5 ; do
  echo "$(date) lorem ipsum"
  sleep 3
done
```
__EOT__
