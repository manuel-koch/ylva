#!/bin/zsh
make build-debug
YLVA=${CARGO_TARGET_DIR:-./target}/debug/ylva

echo "----------------------------------------------------------"
${YLVA} << '__EOT__'
Search for markdown files in the docs directory (non-recursive).
Report one found path per line!
__EOT__

echo "----------------------------------------------------------"
${YLVA} << '__EOT__'
Search for markdown files in the docs directory (recursive).
Report one found path per line!
__EOT__
