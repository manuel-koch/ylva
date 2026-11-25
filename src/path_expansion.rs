// Copyright © 2026 Manuel Koch
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

/// Expand environment variable tokens and a leading tilde in a path string.
///
/// `$NAME` and `${NAME}` (`NAME` = `[A-Za-z_][A-Za-z0-9_]*`) use the supplied
/// lookup. Undefined and malformed tokens stay literal; replacement values are
/// not rescanned for variables. After variable substitution, `~` and `~/`
/// expand through `lookup("HOME")`; `~user` is not expanded.
///
/// This performs string substitution only, without invoking a shell or reading
/// the process environment directly.
pub(crate) fn expand_path_tokens(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    expand_leading_tilde(&substitute_environment_variables(value, lookup), lookup)
}

/// Replace every well-formed `$NAME`/`${NAME}` token in `value` exactly once.
///
/// The output is built only from untouched input characters and resolved
/// values, so a replacement value containing `$OTHER` is emitted verbatim.
fn substitute_environment_variables(
    value: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> String {
    let bytes = value.as_bytes();
    let mut expanded = String::with_capacity(value.len());
    let mut position = 0;
    while position < bytes.len() {
        if bytes[position] != b'$' {
            let character = value[position..]
                .chars()
                .next()
                .expect("every position starts a character");
            expanded.push(character);
            position += character.len_utf8();
            continue;
        }
        match read_variable_reference(bytes, position) {
            Some((name, length)) => {
                match lookup(name) {
                    Some(replacement) => expanded.push_str(&replacement),
                    None => expanded.push_str(&value[position..position + length]),
                }
                position += length;
            }
            None => {
                expanded.push('$');
                position += 1;
            }
        }
    }
    expanded
}

/// Read a `$NAME` or `${NAME}` reference starting at `position` (the index
/// of the `$`), returning the variable name and the number of bytes the
/// reference occupies from `position`.
///
/// Returns `None` when no valid reference starts there — an empty name, a
/// name that does not start with `[A-Za-z_]`, an unterminated `${…`, or a
/// bare `$` — so the caller leaves that `$` literal.
fn read_variable_reference(bytes: &[u8], position: usize) -> Option<(&str, usize)> {
    let (name_start, name_end, consumed_end) = if bytes.get(position + 1) == Some(&b'{') {
        let name_start = position + 2;
        let mut name_end = name_start;
        while name_end < bytes.len() && is_variable_name_byte(bytes[name_end]) {
            name_end += 1;
        }
        if name_end == name_start
            || !is_variable_name_start(bytes[name_start])
            || bytes.get(name_end) != Some(&b'}')
        {
            return None;
        }
        (name_start, name_end, name_end + 1)
    } else {
        let name_start = position + 1;
        if name_start >= bytes.len() || !is_variable_name_start(bytes[name_start]) {
            return None;
        }
        let mut name_end = name_start;
        while name_end < bytes.len() && is_variable_name_byte(bytes[name_end]) {
            name_end += 1;
        }
        (name_start, name_end, name_end)
    };
    let name = std::str::from_utf8(&bytes[name_start..name_end]).ok()?;
    Some((name, consumed_end - position))
}

/// First byte of a variable name: `[A-Za-z_]`.
fn is_variable_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// Any byte of a variable name: `[A-Za-z0-9_]`.
fn is_variable_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Expand a leading `~` or `~/` prefix through `lookup("HOME")`.
///
/// `~user/…` is deliberately unsupported: expanding it would require parsing
/// the operating system's user database for no benefit, so the value stays
/// untouched.
fn expand_leading_tilde(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let Some(rest) = value.strip_prefix('~') else {
        return value.to_string();
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        return value.to_string();
    }
    match lookup("HOME") {
        Some(home) => format!("{home}{rest}"),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::expand_path_tokens;

    fn lookup_from<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            values
                .iter()
                .find(|(variable_name, _)| *variable_name == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn expand_path_tokens_replaces_defined_variables() {
        let lookup = lookup_from(&[("YLVA_HOME", "/home/user/.ylva")]);

        assert_eq!(
            expand_path_tokens("$YLVA_HOME/AGENTS.md", &lookup),
            "/home/user/.ylva/AGENTS.md"
        );
        assert_eq!(
            expand_path_tokens("${YLVA_HOME}/AGENTS.md", &lookup),
            "/home/user/.ylva/AGENTS.md"
        );
        assert_eq!(
            expand_path_tokens("pre$YLVA_HOME_post", &lookup),
            "pre$YLVA_HOME_post",
            "an underscore is part of the variable name, not a delimiter"
        );
    }

    #[test]
    fn expand_path_tokens_leaves_undefined_or_malformed_tokens_literal() {
        let lookup = lookup_from(&[]);

        for value in [
            "$UNDEFINED/file.md",
            "${UNDEFINED}/file.md",
            "${unterminated",
            "${}",
            "$1/file.md",
            "cost$5/file.md",
            "$",
            "src/main.rs",
        ] {
            assert_eq!(expand_path_tokens(value, &lookup), value, "for {value:?}");
        }
    }

    #[test]
    fn expand_path_tokens_substitutes_once_without_rescanning() {
        let lookup = lookup_from(&[("FIRST", "$SECOND"), ("SECOND", "resolved")]);

        assert_eq!(
            expand_path_tokens("$FIRST", &lookup),
            "$SECOND",
            "a substituted value is never rescanned for further tokens"
        );
    }

    #[test]
    fn expand_path_tokens_expands_only_a_leading_tilde() {
        let lookup = lookup_from(&[("HOME", "/home/user")]);

        assert_eq!(expand_path_tokens("~", &lookup), "/home/user");
        assert_eq!(
            expand_path_tokens("~/notes.md", &lookup),
            "/home/user/notes.md"
        );
        assert_eq!(
            expand_path_tokens("~user/notes.md", &lookup),
            "~user/notes.md",
            "`~user` expansion is deliberately unsupported"
        );
        assert_eq!(
            expand_path_tokens("docs/~template.md", &lookup),
            "docs/~template.md",
            "only a leading tilde is expanded"
        );
    }

    #[test]
    fn expand_path_tokens_without_any_lookup_returns_the_value_unchanged() {
        let lookup = lookup_from(&[]);

        assert_eq!(expand_path_tokens("src/main.rs", &lookup), "src/main.rs");
        assert_eq!(expand_path_tokens("~/notes.md", &lookup), "~/notes.md");
        assert_eq!(
            expand_path_tokens("$YLVA_HOME/AGENTS.md", &lookup),
            "$YLVA_HOME/AGENTS.md"
        );
    }
}
