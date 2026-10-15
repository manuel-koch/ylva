# Bash approval

How can we find out if a given `bash` command is `readonly` and
could be auto-approved ?

There are commands that are in basically `readonly`, but have certain
arguments that can make them `write` or `edit` something:

- `find . -name "*.md" -exec rm {} +`
- `sed -i '' 's/old_string/new_string/g' file.txt`

But even if we find a list of assumed `readonly` commands couldn't anybody
( even the LLM ), just create an executable that is placed in the
PATH and performs "evil" side-effects despite its well-known name,
like an evil-twin of `cat` ?

Such a positive list of `readonly` commands is at best a convinience
for auto-approval and in no way a security/sandbox feature, isn't it ?

See [bash tool config](../../.ylva/profiles/other/config.toml)
for some command rules.
