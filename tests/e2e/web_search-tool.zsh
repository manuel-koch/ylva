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

#
# Manual e2e check for the `web_search` tool — run by hand, never by a Make
# gate. The profile picks its backend through YLVA_WEB_SEARCH_BACKEND in the
# gitignored tests/e2e/custom/profiles/default/.env:
#
#   YLVA_WEB_SEARCH_BACKEND=searxng   # default: the local SearXNG instance
#   YLVA_WEB_SEARCH_BACKEND=brave     # Brave Search; the same .env must also
#                                     # define BRAVE_API_KEY (the token is
#                                     # read through ${BRAVE_API_KEY}
#                                     # interpolation only, never from the
#                                     # process environment implicitly)
YLVA=${CARGO_TARGET_DIR:-./target}/debug/ylva
ROOT_DIR=$(git rev-parse --show-toplevel)
LOG_DIR=${ROOT_DIR}/tmp
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
            --auto-approve-web-search \
            | tee ${OUT_PATH}
}

function seperator() {
    echo
    echo "----------------------------------------------------------"
    echo
}

ylva << '__EOT__'
Do a web research about agent-harnesses, overview of available agents,
best performance/usability.
__EOT__

seperator

ylva << '__EOT__'
Do a web research about search-website that can be used to search for
given terms.
Check whether they can return structured data or just html.
Check whether they have contrains like rate-limits, bot-detection,
need authentification/api-key etc.
__EOT__
