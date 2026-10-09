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

/// A top-level application that can be run to completion.
///
/// The returned `u8` is an exit code: `0` means clean exit; errors propagate as `Err`.
/// `main.rs` converts the result to `std::process::exit`.
pub trait App {
    fn run(self) -> impl std::future::Future<Output = anyhow::Result<u8>> + Send;
}

/// Final persisted identity, independent of application success or failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionExitSnapshot {
    pub session_id: uuid::Uuid,
    pub title: String,
}

pub type SessionExitHandle = std::sync::Arc<std::sync::Mutex<Option<SessionExitSnapshot>>>;

pub fn single_line(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() || character == '\u{2028}' || character == '\u{2029}' {
                ' '
            } else {
                character
            }
        })
        .collect()
}

pub(crate) struct SessionExitGuard {
    pub current_store: std::sync::Arc<std::sync::Mutex<Option<crate::Store>>>,
    pub snapshot: Option<SessionExitHandle>,
}

impl Drop for SessionExitGuard {
    fn drop(&mut self) {
        if let Some(snapshot) = &self.snapshot {
            let store = self.current_store.lock().expect("current store mutex");
            *snapshot.lock().expect("exit snapshot mutex") =
                store.as_ref().map(|store| SessionExitSnapshot {
                    session_id: store.session_id(),
                    title: store.title(),
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exit_snapshot_tracks_rollover_and_persisted_title_on_error() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::Store::create(
            directory.path(),
            directory.path(),
            uuid::Uuid::new_v4(),
            "mock",
            "mock",
            None,
        )
        .unwrap();
        let current_store = std::sync::Arc::new(std::sync::Mutex::new(Some(store.clone())));
        let snapshot = SessionExitHandle::default();
        let guard = SessionExitGuard {
            current_store: current_store.clone(),
            snapshot: Some(snapshot.clone()),
        };
        let replacement = store.start_new_session("mock", "mock", None, None).unwrap();
        replacement
            .set_title("Replacement".to_string())
            .await
            .unwrap();
        *current_store.lock().unwrap() = Some(replacement.clone());
        drop(guard);
        assert_eq!(
            *snapshot.lock().unwrap(),
            Some(SessionExitSnapshot {
                session_id: replacement.session_id(),
                title: "Replacement".into()
            })
        );
    }
}
