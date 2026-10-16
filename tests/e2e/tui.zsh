#!/bin/zsh
# Copyright © 2026 Manuel Koch
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

YLVA=${CARGO_TARGET_DIR:-./target}/debug/ylva
ROOT_DIR=$(git rev-parse --show-toplevel)
LOG_DIR=${ROOT_DIR}/.ylva/logs
LOG_FORMAT=text
PREFIX=$(basename $0 .zsh)

cd ${ROOT_DIR}

make build-debug

PREFIX_TIMESTAMPED="${LOG_DIR}/${PREFIX}_$(date +%Y%m%d%H%M%S)"
LOG_PATH="${PREFIX_TIMESTAMPED}.log"
OUT_PATH="${PREFIX_TIMESTAMPED}.txt"
export YLVA_REQUEST_DUMP_PATH=${PREFIX_TIMESTAMPED}.json
export YLVA_TOKEN_USAGE_DUMP_PATH=${LOG_DIR}/token_stats.jsonl
${YLVA} --log-file ${LOG_PATH} \
        --log-level DEBUG --log-format ${LOG_FORMAT} \
        --auto-approve-spawn "$@"
