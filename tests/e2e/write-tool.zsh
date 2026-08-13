#!/bin/zsh
make build-debug
YLVA=${CARGO_TARGET_DIR:-./target}/debug/ylva

echo "----------------------------------------------------------"
${YLVA} << '__EOT__'
Create file test.txt with the following content:

```text
Lorem ipsum hello world!
```
__EOT__
