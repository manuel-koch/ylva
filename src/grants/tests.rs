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

use std::path::{Path, PathBuf};

use super::*;

#[test]
fn from_stored_rejects_missing_and_surplus_attributes() {
    assert!(Grant::from_stored(GrantTool::Read, None, None).is_err());
    assert!(Grant::from_stored(GrantTool::Read, Some("n"), Some("./a")).is_err());
    assert!(Grant::from_stored(GrantTool::LoadSkill, None, Some("./a")).is_err());
    assert!(Grant::from_stored(GrantTool::LoadSkill, Some("s"), None).is_err());
    assert!(Grant::from_stored(GrantTool::WebSearch, None, Some("x")).is_err());
    assert!(Grant::from_stored(GrantTool::SpawnSubagent, Some("x"), None).is_err());
    assert!(Grant::from_stored(GrantTool::WebFetch, None, Some("not a url")).is_err());
}

#[test]
fn tool_names_round_trip() {
    for tool in GrantTool::ALL {
        assert_eq!(GrantTool::from_name(tool.name()), Some(tool));
    }
    assert_eq!(GrantTool::from_name("clarify"), None);
}

#[test]
fn covers_matches_only_the_same_subject_kind() {
    let root = Path::new("/work");
    let read = Grant::from_stored(GrantTool::Read, None, Some("./src/*")).unwrap();
    assert!(read.covers(&GrantSubject::Path(PathBuf::from("/work/src/a.rs")), root));
    assert!(!read.covers(&GrantSubject::Path(PathBuf::from("/work/other/a.rs")), root));
    assert!(!read.covers(&GrantSubject::Unconditional, root));

    let skill =
        Grant::from_stored(GrantTool::LoadSkill, Some("s"), Some("./skills/s/SKILL.md")).unwrap();
    let skill_subject = |name: &str| GrantSubject::Skill {
        name: name.to_string(),
        source: PathBuf::from("/work/skills/s/SKILL.md"),
    };
    assert!(skill.covers(&skill_subject("s"), root));
    assert!(!skill.covers(&skill_subject("t"), root));
}

#[test]
fn reference_hides_the_rule_text() {
    let grant = Grant::from_stored(GrantTool::Read, None, Some("./secret-dir/*")).unwrap();
    let reference = grant.reference(GrantLifetime::Persistent);
    assert!(reference.as_str().starts_with("persistent:read:"));
    assert!(!reference.as_str().contains("secret"));
    assert_ne!(
        reference,
        Grant::from_stored(GrantTool::Read, None, Some("./other/*"))
            .unwrap()
            .reference(GrantLifetime::Persistent)
    );
}
