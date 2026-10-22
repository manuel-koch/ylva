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
            --auto-approve-spawn \
            | tee ${OUT_PATH}
}

function seperator() {
    echo
    echo "----------------------------------------------------------"
    echo
}

ylva << '__EOT__'
Run the follow task in a subagent and let it report its findings back:

Analyze the TUI functionality implemented in source code
and list currently implemented features in a table with brief descriptions.
__EOT__

seperator

ylva << '__EOT__'
Run the follow task in a subagent and let it report its findings back:

Analyze the TUI functionality implemented in source code
and list currently implemented features in a table with brief descriptions.
ONLY use "bash" tool to search for relevant files!
__EOT__

seperator

ylva << '__EOT__'
Run the follow task in a subagent and let it report its findings back:

Analyze the TUI functionality implemented in source code
and list currently implemented features brief description.

Format result as structured JSON:
```json
{
  "analysed_files": [
    {
      "path": "<source-code-file>",
      "features": [
        "<description of the feature a in 1-3 sentences>",
        "<description of the feature b in 1-3 sentences>",
        ...
        ]
    },
    ...
  ]
}
__EOT__

seperator

ylva << '__EOT__'
Discover the project structure related to agent tool implementations.
Provide a 2-3 sentence summary for every source file.
Run the summary task of each file in separate subagent
and let it report its findings back.

Format the result using markdown headers (per file) and the
related file summary paragraph.
__EOT__
