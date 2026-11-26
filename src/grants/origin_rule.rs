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

//! Origin matcher for `web_fetch`: scheme, host and effective port of the
//! initial URL. Redirects can reach other origins; the sandbox's deny and
//! private-address checks still apply to every hop.

use reqwest::Url;

use super::GrantError;

/// A validated origin rule such as `https://docs.example:443`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginRule {
    text: String,
}

impl OriginRule {
    /// Validate a stored or user-edited origin. A rule must be an origin
    /// only: no credentials, path, query or fragment.
    pub fn parse(text: &str) -> Result<OriginRule, GrantError> {
        if text.contains(['\n', '\r']) {
            return Err(GrantError::new("the origin rule must be a single line"));
        }
        let url = Url::parse(text)
            .map_err(|error| GrantError::new(format!("invalid origin '{text}': {error}")))?;
        if !url.username().is_empty()
            || url.password().is_some()
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(GrantError::new(
                "an origin rule is scheme, host and optional port only (no credentials, path, \
                 query or fragment)",
            ));
        }
        OriginRule::from_url(&url)
            .ok_or_else(|| GrantError::new("an origin rule needs an http or https URL with a host"))
    }

    /// The origin of a fetched URL; `None` for anything that is not an
    /// absolute `http`/`https` URL with a host.
    pub fn origin_of(url: &str) -> Option<String> {
        OriginRule::from_url(&Url::parse(url).ok()?).map(|rule| rule.text)
    }

    fn from_url(url: &Url) -> Option<OriginRule> {
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        let host = url.host_str()?;
        let port = url.port_or_known_default()?;
        Some(OriginRule {
            text: format!("{}://{host}:{port}", url.scheme()),
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether `origin` (as produced by [`OriginRule::origin_of`]) equals the rule.
    pub fn matches(&self, origin: &str) -> bool {
        self.text == origin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_is_normalized_with_the_effective_port() {
        assert_eq!(
            OriginRule::parse("https://Docs.example").unwrap().text(),
            "https://docs.example:443"
        );
        assert_eq!(
            OriginRule::parse("http://docs.example").unwrap().text(),
            "http://docs.example:80"
        );
    }

    #[test]
    fn origin_matches_any_path_but_not_other_scheme_host_or_port() {
        let rule = OriginRule::parse("https://docs.example:443").unwrap();
        let origin = |url: &str| OriginRule::origin_of(url).unwrap();
        assert!(rule.matches(&origin("https://docs.example/a?b=c")));
        assert!(rule.matches(&origin("https://docs.example:443/")));
        assert!(rule.matches(&origin("https://docs.example/a#section")));
        assert!(rule.matches(&origin("https://docs.example/a?b=c#section")));
        assert!(rule.matches(&origin("https://docs.example#top")));
        assert!(!rule.matches(&origin("http://docs.example/a")));
        assert!(!rule.matches(&origin("https://other.example/a")));
        assert!(!rule.matches(&origin("https://docs.example:8443/a")));
    }

    #[test]
    fn invalid_rules_are_rejected() {
        for rule in [
            "docs.example",
            "ftp://docs.example",
            "https://user@docs.example",
            "https://docs.example/path",
            "https://docs.example?q=1",
            "https://docs.example#top",
            "https://docs.example/#top",
            "https://docs.example\n",
            "",
        ] {
            assert!(OriginRule::parse(rule).is_err(), "{rule:?}");
        }
    }

    #[test]
    fn non_http_urls_have_no_origin() {
        assert_eq!(OriginRule::origin_of("file:///etc/hosts"), None);
        assert_eq!(OriginRule::origin_of("not a url"), None);
    }
}
