#!/bin/zsh
make build-debug
YLVA=${CARGO_TARGET_DIR:-./target}/debug/ylva

echo "----------------------------------------------------------"
${YLVA} << '__EOT__'
Read lines 7 to 8 of (big file!) tests/e2e/read-tool.zsh and do a spell check.
Repoort only the findings. Do not change anything!
__EOT__

echo "----------------------------------------------------------"
${YLVA} << '__EOT__'
Read the first 10 characters of tests/e2e/read-tool.zsh.
Report only the plain read text snippet.
__EOT__

echo "----------------------------------------------------------"
${YLVA} << '__EOT__'
Read the first 32 bytes of tests/e2e/read-tool.zsh.
Report only the plain read data snippet.
__EOT__
