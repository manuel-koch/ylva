# Bash approval

How can we find out if a given `bash` command is `readonly` and
could be auto-approved ?

- there are commands that are in basically `readonly`, but have certain
  arguments that can make them `write` or `edit` something:
  - `find . -name "*.md" -exec rm {} +`
  - `sed -i '' 's/old_string/new_string/g' file.txt`

See [bash tool config](../../tests/e2e/custom/profiles/other/config.toml)
for some command rules.
