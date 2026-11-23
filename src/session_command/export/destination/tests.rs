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

use super::publication::{PublishError, Publisher};
use super::*;
use std::io::Read;
use std::os::unix::fs::symlink;

#[test]
fn publication_refuses_a_retargeted_parent_alias() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let chats = Directory::resolve(&root.join("chats")).unwrap();
    fs::create_dir(root.join("original")).unwrap();
    fs::create_dir(root.join("replacement")).unwrap();
    symlink("original", root.join("alias")).unwrap();
    let destination = Destination::standalone(&root.join("alias/export"), false).unwrap();
    fs::remove_file(root.join("alias")).unwrap();
    symlink("replacement", root.join("alias")).unwrap();

    let result = Publisher::new(&chats).standalone(&destination, &b"data"[..], &|| false);

    assert!(matches!(result, Err(PublishError::Failed(_))));
    assert!(!root.join("original/export").exists());
    assert!(!root.join("replacement/export").exists());
}

#[test]
fn publication_rechecks_bundle_emptiness_and_leaves_existing_entries_alone() {
    let fixture = tempfile::tempdir().unwrap();
    let chats = Directory::resolve(&fixture.path().join("chats")).unwrap();
    let path = fixture.path().join("bundle");
    fs::create_dir(&path).unwrap();
    let directory = Directory::bundle(&path).unwrap();
    let existing = path.join(".keep");
    fs::write(&existing, b"created after preflight").unwrap();

    let result = Publisher::new(&chats).bundle(&directory, &|| false);

    assert!(result.unwrap_err().to_string().contains("no longer empty"));
    assert_eq!(fs::read(existing).unwrap(), b"created after preflight");
    assert_eq!(fs::read_dir(path).unwrap().count(), 1);
}

#[test]
fn publication_refuses_a_file_that_disappeared_or_changed_after_preflight() {
    let fixture = tempfile::tempdir().unwrap();
    let chats = Directory::resolve(&fixture.path().join("chats")).unwrap();
    let mut publisher = Publisher::new(&chats);
    for replacement in [None, Some(b"new contents".as_slice())] {
        let path = fixture.path().join("export");
        fs::write(&path, b"old contents").unwrap();
        let destination = Destination::standalone(&path, true).unwrap();
        if let Some(replacement) = replacement {
            fs::write(&path, replacement).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        let result = publisher.standalone(&destination, &b"export bytes"[..], &|| false);
        assert!(matches!(result, Err(PublishError::Failed(_))));
        assert_eq!(fs::read(path).ok().as_deref(), replacement);
    }
}

#[test]
fn publication_leaves_created_directories_and_continues_after_a_late_conflict() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let chats = Directory::resolve(&root.join("chats")).unwrap();
    let mut publisher = Publisher::new(&chats);
    let output = root.join("new/exports");
    let path = output.join("first");
    let first = Destination::standalone(&path, true).unwrap();
    let second = Destination::standalone(&output.join("second"), false).unwrap();
    let mut changed = false;
    let source = io::repeat(0).take(16).chain(CallbackReader(|| {
        if !changed {
            fs::write(&path, b"another writer")?;
            changed = true;
        }
        Ok(())
    }));

    let result = publisher.standalone(&first, source, &|| false);
    assert!(matches!(result, Err(PublishError::Failed(_))));
    assert_eq!(fs::read(path).unwrap(), b"another writer");
    assert_eq!(fs::read_dir(&output).unwrap().count(), 1);
    assert_eq!(
        publisher.created_paths,
        [
            root.canonicalize().unwrap().join("new"),
            output.canonicalize().unwrap()
        ],
    );
    publisher
        .standalone(&second, &b"independent output"[..], &|| false)
        .unwrap();
    assert_eq!(
        fs::read(output.join("second")).unwrap(),
        b"independent output"
    );
}

struct CallbackReader<F>(F);

impl<F: FnMut() -> io::Result<()>> Read for CallbackReader<F> {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        (self.0)()?;
        Ok(0)
    }
}

#[test]
fn publication_is_uncertain_when_the_post_publish_directory_sync_fails() {
    let fixture = tempfile::tempdir().unwrap();
    let chats = Directory::resolve(&fixture.path().join("chats")).unwrap();
    let path = fixture.path().join("output");
    let destination = Destination::standalone(&path, false).unwrap();
    let mut publisher = Publisher::new(&chats);
    publisher.sync_published_directory = |_| Err(io::Error::other("sync failed"));

    let result = publisher.standalone(&destination, &b"published"[..], &|| false);

    assert!(matches!(result, Err(PublishError::Uncertain(_))));
    assert_eq!(fs::read(path).unwrap(), b"published");
}
