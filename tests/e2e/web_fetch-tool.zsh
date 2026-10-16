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

function ylva() {
    PREFIX_TIMESTAMPED="${LOG_DIR}/${PREFIX}_$(date +%Y%m%d%H%M%S)"
    LOG_PATH="${PREFIX_TIMESTAMPED}.log"
    OUT_PATH="${PREFIX_TIMESTAMPED}.txt"
    export YLVA_REQUEST_DUMP_PATH=${PREFIX_TIMESTAMPED}.json
    export YLVA_TOKEN_USAGE_DUMP_PATH=${LOG_DIR}/token_stats.jsonl
    ${YLVA} --no-tui --color=yes \
            --log-file ${LOG_PATH} \
            --log-level DEBUG --log-format ${LOG_FORMAT} \
            --auto-approve-web-fetch \
            | tee ${OUT_PATH}
}

function seperator() {
    echo
    echo "----------------------------------------------------------"
    echo
}

ylva << '__EOT__'
Do a web research of GitOver project at `https://github.com/manuel-koch/gitover-rust-tui`.
__EOT__

seperator

ylva << '__EOT__'
Read `https://www.bundestag.de/` and report a summary of the page.
__EOT__

seperator

ylva << '__EOT__'
Fetch image using `web_fetch` and describe its content: `https://i.ytimg.com/vi/0iNrGpwZwog/sddefault.jpg`.
__EOT__

seperator

ylva << '__EOT__'
Do a search on Github using `web_fetch` and this URL template for url-encoded terms:
`https://api.github.com/search/issues?per_page=3&page=1&q=<terms>+language:rust`.
Search for "agent harness" and report repository URLs from the result.
__EOT__
