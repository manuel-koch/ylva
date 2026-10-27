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

//! Large pasted texts shown as one-line placeholder markers in the draft.

/// Pasted texts longer than this many characters are collapsed to a marker.
pub const LARGE_PASTE_CHAR_THRESHOLD: usize = 1_000;

/// One collapsed paste: the marker shown in the draft and the full text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastedBlob {
    pub marker: String,
    pub text: String,
}

/// Collapsed pastes of the current draft, in paste order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PastedBlobs {
    blobs: Vec<PastedBlob>,
}

impl PastedBlobs {
    /// Store `text` and return the marker to insert into the draft when it
    /// has more than `threshold` characters; `None` when it is inserted as-is.
    pub fn collapse(&mut self, text: &str, threshold: usize) -> Option<String> {
        let character_count = text.chars().count();
        if character_count <= threshold {
            return None;
        }
        let marker = format!(
            "[Pasted text #{}: {} lines, {} chars]",
            self.blobs.len() + 1,
            group_thousands(text.split('\n').count()),
            group_thousands(character_count),
        );
        self.blobs.push(PastedBlob {
            marker: marker.clone(),
            text: text.to_owned(),
        });
        Some(marker)
    }

    /// Replace every marker still present verbatim in `draft` by its text.
    /// Markers the user edited are plain text and stay untouched.
    pub fn expand(&self, draft: &str) -> String {
        self.blobs.iter().fold(draft.to_owned(), |expanded, blob| {
            expanded.replace(&blob.marker, &blob.text)
        })
    }

    pub fn clear(&mut self) {
        self.blobs.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }
}

fn group_thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_only_above_threshold() {
        let mut blobs = PastedBlobs::default();
        assert_eq!(blobs.collapse(&"a".repeat(10), 10), None);
        assert!(blobs.is_empty());
        assert_eq!(
            blobs.collapse(&"a\n".repeat(1_500), 10).as_deref(),
            Some("[Pasted text #1: 1,501 lines, 3,000 chars]")
        );
    }

    #[test]
    fn distinct_markers_and_expansion() {
        let mut blobs = PastedBlobs::default();
        let first = blobs.collapse("first text", 3).unwrap();
        let second = blobs.collapse("second text", 3).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            blobs.expand(&format!("a {first} b {second}")),
            "a first text b second text"
        );
    }

    #[test]
    fn edited_marker_is_not_expanded() {
        let mut blobs = PastedBlobs::default();
        let marker = blobs.collapse("secret body", 3).unwrap();
        let edited = marker.replace("#1", "#9");
        assert_eq!(blobs.expand(&edited), edited);
        blobs.clear();
        assert_eq!(blobs.expand(&marker), marker);
    }
}
