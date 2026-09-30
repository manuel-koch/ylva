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

//! Host-aware matching for `network_request` rule `target` patterns.
//!
//! [`NetworkTargetPattern`] is the compiled form of a `target` parameter.
//! It is matched against the request's `scheme://host[:port]` network-sandbox
//! match key (see [`crate::http::network_target`]) — never against a path —
//! so its vocabulary is host-shaped, not path-glob-shaped:
//!
//! - `SCHEME` — a literal scheme (case-insensitive) or `*` (any scheme).
//!   Omitted entirely the pattern defaults to `https`, which can only ever
//!   narrow a match (`http://…` still falls through to the tool's floor).
//! - `HOST` — dot-separated labels; each label is a literal (case-insensitive),
//!   `*` (exactly one label), or `**` (one or more labels). A bracketed IPv6
//!   host is one literal label.
//! - `PORT` — `*` (any port), or digits. Ports compare *effectively*: an
//!   absent port equals the scheme's default (`https://host` ≡
//!   `https://host:443`), so patterns and keys match regardless of whether
//!   the default port is spelled out.
//!
//! The bare catch-alls `*`/`**` are rejected: "match every call" is spelled
//! by omitting `target`, so it can never hide behind a defaulted scheme.

/// A compiled `network_request` `target` pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkTargetPattern {
    scheme: SchemePattern,
    host: Vec<LabelPattern>,
    port: PortPattern,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SchemePattern {
    Any,
    Literal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LabelPattern {
    /// A literal DNS label (lowercased at parse time).
    Literal(String),
    /// `*` — exactly one label.
    Single,
    /// `**` — one or more labels.
    Multi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PortPattern {
    /// The pattern spells no port: matches a key without a port or with the
    /// scheme's default port.
    Absent,
    /// `*` — any port.
    Any,
    /// Digits: matches the key's effective port.
    Literal(u16),
}

/// Why a `target` pattern could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum NetworkTargetPatternError {
    #[error("a target must name a host; omit `target` to match every call (got '{pattern}')")]
    CatchAllWithoutHost { pattern: String },
    #[error(
        "'{scheme}' is not a valid scheme pattern; use a literal scheme or `*` \
         (got '{pattern}')"
    )]
    InvalidScheme { pattern: String, scheme: String },
    #[error(
        "no valid host in target (got '{pattern}'); a target names `SCHEME://HOST[:PORT]`, \
         never a path"
    )]
    EmptyHost { pattern: String },
    #[error(
        "'{label}' is not a valid host label; use a literal label, `*` (exactly one label), \
         or `**` (one or more labels) (got '{pattern}')"
    )]
    InvalidLabel { pattern: String, label: String },
    #[error("'{port}' is not a valid port; use `*` or digits (got '{pattern}')")]
    InvalidPort { pattern: String, port: String },
}

impl NetworkTargetPattern {
    /// Compile a `target` pattern. See the module docs for the vocabulary.
    pub(crate) fn parse(pattern: &str) -> Result<Self, NetworkTargetPatternError> {
        if pattern == "*" || pattern == "**" {
            return Err(NetworkTargetPatternError::CatchAllWithoutHost {
                pattern: pattern.to_string(),
            });
        }
        let (scheme_part, rest) = match pattern.split_once("://") {
            Some((scheme, rest)) => (Some(scheme), rest),
            None => (None, pattern),
        };
        let scheme = match scheme_part {
            None => SchemePattern::Literal("https".to_string()),
            Some("*") => SchemePattern::Any,
            Some(scheme) => {
                if is_literal_token(scheme) {
                    SchemePattern::Literal(scheme.to_ascii_lowercase())
                } else {
                    return Err(NetworkTargetPatternError::InvalidScheme {
                        pattern: pattern.to_string(),
                        scheme: scheme.to_string(),
                    });
                }
            }
        };
        let (host_part, port) = split_host_port(pattern, rest)?;
        let host = parse_host(pattern, host_part)?;
        Ok(Self { scheme, host, port })
    }

    /// Match against a `scheme://host[:port]` match key. Unparseable keys
    /// never match (fail-safe: the tool's approval floor applies).
    pub(crate) fn matches(&self, call_target: &str) -> bool {
        let Some((key_scheme, key_host, key_port)) = parse_match_key(call_target) else {
            return false;
        };
        scheme_matches(&self.scheme, &key_scheme)
            && labels_match(&self.host, &key_host)
            && port_matches(&self.port, &key_scheme, key_port)
    }
}

/// The default port of `scheme`, used for effective-port comparison.
fn default_port(scheme: &str) -> Option<u16> {
    match scheme.to_ascii_lowercase().as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

fn scheme_matches(pattern: &SchemePattern, key_scheme: &str) -> bool {
    match pattern {
        SchemePattern::Any => true,
        SchemePattern::Literal(literal) => literal.eq_ignore_ascii_case(key_scheme),
    }
}

/// Sequence-match pattern labels against key labels: `*` consumes exactly one
/// label, `**` one or more, literals compare case-insensitively.
fn labels_match(pattern: &[LabelPattern], key_labels: &[String]) -> bool {
    match pattern.split_first() {
        None => key_labels.is_empty(),
        Some((head, tail)) => match head {
            LabelPattern::Single => !key_labels.is_empty() && labels_match(tail, &key_labels[1..]),
            LabelPattern::Multi => {
                (1..=key_labels.len()).any(|consumed| labels_match(tail, &key_labels[consumed..]))
            }
            LabelPattern::Literal(literal) => {
                key_labels
                    .first()
                    .is_some_and(|label| literal.eq_ignore_ascii_case(label))
                    && labels_match(tail, &key_labels[1..])
            }
        },
    }
}

/// Effective-port matching: an absent pattern port matches keys without a
/// port or with the scheme's default port; a literal matches the key's
/// effective port (explicit or scheme default).
fn port_matches(pattern: &PortPattern, key_scheme: &str, key_port: Option<u16>) -> bool {
    match pattern {
        PortPattern::Any => true,
        PortPattern::Absent => key_port.is_none_or(|port| Some(port) == default_port(key_scheme)),
        PortPattern::Literal(literal) => {
            key_port.or_else(|| default_port(key_scheme)) == Some(*literal)
        }
    }
}

/// Split `rest` (everything after `scheme://`) into the host part and the
/// port pattern. Bracketed IPv6 hosts keep their brackets as one label.
fn split_host_port<'a>(
    pattern: &str,
    rest: &'a str,
) -> Result<(&'a str, PortPattern), NetworkTargetPatternError> {
    if rest.contains('/') || rest.contains('#') || rest.contains('@') {
        return Err(NetworkTargetPatternError::EmptyHost {
            pattern: pattern.to_string(),
        });
    }
    if let Some(bracketed) = rest.strip_prefix('[') {
        let Some((host_inner, remainder)) = bracketed.split_once(']') else {
            return Err(NetworkTargetPatternError::InvalidLabel {
                pattern: pattern.to_string(),
                label: rest.to_string(),
            });
        };
        let host = &rest[..=host_inner.len() + 1];
        let port = match remainder {
            "" => PortPattern::Absent,
            _ => parse_port(pattern, remainder.strip_prefix(':').unwrap_or(remainder))?,
        };
        return Ok((host, port));
    }
    match rest.rsplit_once(':') {
        Some((host, port_part)) => Ok((host, parse_port(pattern, port_part)?)),
        None => Ok((rest, PortPattern::Absent)),
    }
}

/// Parse the port text after `:` (`*` = any port, digits = a literal port).
fn parse_port(pattern: &str, port_part: &str) -> Result<PortPattern, NetworkTargetPatternError> {
    if port_part == "*" {
        return Ok(PortPattern::Any);
    }
    if !port_part.is_empty() && port_part.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(port) = port_part.parse::<u16>() {
            return Ok(PortPattern::Literal(port));
        }
    }
    Err(NetworkTargetPatternError::InvalidPort {
        pattern: pattern.to_string(),
        port: port_part.to_string(),
    })
}

/// Parse the host part into labels (dot-separated, or one bracketed IPv6
/// label).
fn parse_host(pattern: &str, host: &str) -> Result<Vec<LabelPattern>, NetworkTargetPatternError> {
    if host.is_empty() {
        return Err(NetworkTargetPatternError::EmptyHost {
            pattern: pattern.to_string(),
        });
    }
    if host.starts_with('[') {
        let inner = host
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .ok_or_else(|| NetworkTargetPatternError::InvalidLabel {
                pattern: pattern.to_string(),
                label: host.to_string(),
            })?;
        if inner.is_empty() || inner.contains(['[', ']', '*']) {
            return Err(NetworkTargetPatternError::InvalidLabel {
                pattern: pattern.to_string(),
                label: host.to_string(),
            });
        }
        return Ok(vec![LabelPattern::Literal(host.to_ascii_lowercase())]);
    }
    host.split('.')
        .map(|label| parse_label(pattern, label))
        .collect()
}

fn parse_label(pattern: &str, label: &str) -> Result<LabelPattern, NetworkTargetPatternError> {
    match label {
        "*" => return Ok(LabelPattern::Single),
        "**" => return Ok(LabelPattern::Multi),
        _ => {}
    }
    if label.is_empty() || !is_literal_token(label) {
        return Err(NetworkTargetPatternError::InvalidLabel {
            pattern: pattern.to_string(),
            label: label.to_string(),
        });
    }
    Ok(LabelPattern::Literal(label.to_ascii_lowercase()))
}

/// A literal scheme or label token: non-empty, no wildcards and no URL
/// structural characters.
fn is_literal_token(token: &str) -> bool {
    !token.is_empty() && !token.contains(['*', '?', '[', ']', '/', '\\', ':', '@', '#', ' '])
}

/// Parse a `scheme://host[:port]` match key (produced by
/// [`crate::http::network_target`]). Returns `None` for anything
/// unexpected — such a key can never match.
fn parse_match_key(key: &str) -> Option<(String, Vec<String>, Option<u16>)> {
    let (scheme, rest) = key.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() {
        return None;
    }
    let (host, port) = if let Some(bracketed) = rest.strip_prefix('[') {
        let (host_inner, remainder) = bracketed.split_once(']')?;
        if host_inner.is_empty() {
            return None;
        }
        let host = rest[..=host_inner.len() + 1].to_string();
        let port = match remainder {
            "" => None,
            _ => {
                let digits = remainder.strip_prefix(':')?;
                Some(digits.parse::<u16>().ok()?)
            }
        };
        (host, port)
    } else {
        match rest.rsplit_once(':') {
            Some((host, digits)) => {
                if host.is_empty() || digits.is_empty() {
                    return None;
                }
                (host.to_string(), Some(digits.parse::<u16>().ok()?))
            }
            None => (rest.to_string(), None),
        }
    };
    let labels = if host.starts_with('[') {
        vec![host]
    } else {
        host.split('.').map(str::to_string).collect()
    };
    if labels.iter().any(|label| label.is_empty()) {
        return None;
    }
    Some((scheme.to_string(), labels, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, key: &str) -> bool {
        NetworkTargetPattern::parse(pattern)
            .unwrap_or_else(|error| panic!("pattern '{pattern}' must compile: {error}"))
            .matches(key)
    }

    fn parse_error(pattern: &str) -> NetworkTargetPatternError {
        NetworkTargetPattern::parse(pattern).expect_err("pattern must be rejected")
    }

    #[test]
    fn literal_scheme_and_host_match_only_that_endpoint() {
        assert!(matches("https://github.com", "https://github.com"));
        assert!(!matches("https://github.com", "http://github.com"));
        assert!(!matches("https://github.com", "https://gitlab.com"));
        assert!(!matches(
            "https://github.com",
            "https://github.com.evil.example"
        ));
    }

    #[test]
    fn a_schemeless_pattern_defaults_to_https() {
        assert!(matches("github.com", "https://github.com"));
        assert!(!matches("github.com", "http://github.com"));
        assert!(matches("*.github.com", "https://api.github.com"));
        assert!(!matches("*.github.com", "http://api.github.com"));
    }

    #[test]
    fn single_star_matches_exactly_one_host_label() {
        assert!(matches("https://*.github.com", "https://api.github.com"));
        assert!(!matches("https://*.github.com", "https://github.com"));
        assert!(!matches("https://*.github.com", "https://a.b.github.com"));
    }

    #[test]
    fn double_star_matches_one_or_more_host_labels() {
        assert!(matches("https://**.github.com", "https://api.github.com"));
        assert!(matches("https://**.github.com", "https://a.b.github.com"));
        assert!(matches("https://**.github.com", "https://a.b.c.github.com"));
        assert!(!matches("https://**.github.com", "https://github.com"));
    }

    #[test]
    fn double_star_as_the_whole_host_matches_any_host() {
        assert!(matches("https://**", "https://github.com"));
        assert!(matches("https://**", "https://a.b.github.com"));
        assert!(!matches("https://**", "http://github.com"));
    }

    #[test]
    fn wildcard_scheme_matches_any_scheme() {
        assert!(matches("*://github.com", "https://github.com"));
        assert!(matches("*://github.com", "http://github.com"));
        assert!(!matches("*://github.com", "https://gitlab.com"));
    }

    #[test]
    fn labels_and_schemes_compare_case_insensitively() {
        assert!(matches("HTTPS://GitHub.COM", "https://github.com"));
        assert!(matches("https://*.GITHUB.com", "https://API.github.com"));
    }

    #[test]
    fn absent_port_matches_the_scheme_default_port() {
        assert!(matches("https://github.com", "https://github.com:443"));
        assert!(matches("https://github.com:443", "https://github.com"));
        assert!(matches("http://github.com", "http://github.com:80"));
        assert!(!matches("https://github.com", "https://github.com:8443"));
        assert!(!matches(
            "https://github.com:443",
            "https://github.com:8443"
        ));
    }

    #[test]
    fn literal_port_matches_the_effective_port() {
        assert!(matches("https://github.com:443", "https://github.com"));
        assert!(matches("https://github.com:443", "https://github.com:443"));
        assert!(!matches("https://github.com:443", "http://github.com"));
    }

    #[test]
    fn star_port_matches_any_port_including_absent() {
        assert!(matches("https://github.com:*", "https://github.com"));
        assert!(matches("https://github.com:*", "https://github.com:443"));
        assert!(matches("https://github.com:*", "https://github.com:8443"));
        assert!(!matches("https://github.com:*", "https://gitlab.com"));
    }

    #[test]
    fn patterns_combine_multiple_wildcards() {
        // Two single-label wildcards in one host.
        assert!(matches(
            "https://*.*.example.com",
            "https://a.b.example.com"
        ));
        assert!(!matches("https://*.*.example.com", "https://a.example.com"));
        assert!(!matches(
            "https://*.*.example.com",
            "https://a.b.c.example.com"
        ));
        // Scheme and host wildcards together.
        assert!(matches("*://*.github.com", "https://api.github.com"));
        assert!(matches("*://*.github.com", "http://api.github.com"));
        assert!(!matches("*://*.github.com", "https://a.b.github.com"));
        // Host and port wildcards together.
        assert!(matches("https://*.github.com:*", "https://api.github.com"));
        assert!(matches(
            "https://*.github.com:*",
            "https://api.github.com:8443"
        ));
        assert!(!matches("https://*.github.com:*", "https://a.b.github.com"));
        // `*` and `**` in one host.
        assert!(matches("https://*.**.github.com", "https://x.a.github.com"));
        assert!(matches(
            "https://*.**.github.com",
            "https://x.a.b.github.com"
        ));
        assert!(matches("https://*.**.github.com", "https://a.b.github.com"));
        assert!(!matches("https://*.**.github.com", "https://x.github.com"));
    }

    #[test]
    fn bracketed_ipv6_hosts_are_one_literal_label() {
        assert!(matches("https://[::1]", "https://[::1]"));
        assert!(matches("https://[::1]:8080", "https://[::1]:8080"));
        assert!(!matches("https://[::1]", "https://[::2]"));
        assert!(matches("https://*", "https://[::1]"));
    }

    #[test]
    fn bare_catch_alls_are_rejected_with_guidance() {
        for pattern in ["*", "**"] {
            assert_eq!(
                parse_error(pattern),
                NetworkTargetPatternError::CatchAllWithoutHost {
                    pattern: pattern.to_string()
                }
            );
        }
    }

    #[test]
    fn mixed_wildcard_labels_are_rejected() {
        for pattern in [
            "https://a**b.example.com",
            "https://a*.example.com",
            "https://**b.example.com",
            "https://a?c.example.com",
        ] {
            assert!(
                matches!(
                    parse_error(pattern),
                    NetworkTargetPatternError::InvalidLabel { .. }
                ),
                "{pattern} must be rejected as an invalid label"
            );
        }
    }

    #[test]
    fn invalid_schemes_hosts_and_ports_are_rejected() {
        assert!(matches!(
            parse_error("**://github.com"),
            NetworkTargetPatternError::InvalidScheme { .. }
        ));
        assert!(matches!(
            parse_error("https://"),
            NetworkTargetPatternError::EmptyHost { .. }
        ));
        assert!(matches!(
            parse_error("https://github.com..example.com"),
            NetworkTargetPatternError::InvalidLabel { .. }
        ));
        assert!(matches!(
            parse_error("https://github.com/path"),
            NetworkTargetPatternError::EmptyHost { .. }
        ));
        assert!(matches!(
            parse_error("https://github.com:https"),
            NetworkTargetPatternError::InvalidPort { .. }
        ));
        assert!(matches!(
            parse_error("https://github.com:99999"),
            NetworkTargetPatternError::InvalidPort { .. }
        ));
    }

    #[test]
    fn unparseable_match_keys_never_match() {
        let pattern = NetworkTargetPattern::parse("https://**").expect("compiles");
        for key in ["github.com", "https://", "https://github.com:port", ""] {
            assert!(!pattern.matches(key), "key '{key}' must not match");
        }
    }
}
