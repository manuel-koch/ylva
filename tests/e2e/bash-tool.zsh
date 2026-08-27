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
Run `python3` to find out which version it is and where the python interpreter
is located.
__EOT__

echo "----------------------------------------------------------"
ylva << '__EOT__'
Run the following command exactly like given (as-is, don't re-write/collapse linebreaks ) in bash:
```shell
for n in seq 1 5 ; do
  echo "$(date) lorem ipsum"
  sleep 3
done
```
__EOT__
