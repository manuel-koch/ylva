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

//! Shared runtime grant state: temporary and persistent grants, evaluation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{
    CommandStatement, Grant, GrantLifetime, GrantReference, GrantStore, GrantStoreError,
    GrantSubject, GrantSubjects, GrantTool, SaveOutcome,
};

/// Result of evaluating a tool call against the grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantDecision {
    NotCovered,
    /// Covered; the references name the grants that applied (policy-approved
    /// bash statements need none).
    Covered(Vec<GrantReference>),
}

/// Handle shared by the root conversation and all of its subagents.
pub type SharedGrantState = Arc<GrantState>;

#[derive(Debug, Default)]
struct Inner {
    temporary: Vec<Grant>,
    persistent: Vec<Grant>,
    store: Option<GrantStore>,
}

#[derive(Debug)]
pub struct GrantState {
    inner: Mutex<Inner>,
    generation: tokio::sync::watch::Sender<u64>,
    workspace_root: PathBuf,
}

impl GrantState {
    /// State without persistent storage.
    pub fn new(workspace_root: PathBuf) -> GrantState {
        GrantState {
            inner: Mutex::new(Inner::default()),
            generation: tokio::sync::watch::channel(0).0,
            workspace_root,
        }
    }

    /// State with loaded persistent grants and the store used to append more.
    pub fn with_persistent(
        workspace_root: PathBuf,
        persistent: Vec<Grant>,
        store: Option<GrantStore>,
    ) -> GrantState {
        GrantState {
            inner: Mutex::new(Inner {
                temporary: Vec::new(),
                persistent,
                store,
            }),
            generation: tokio::sync::watch::channel(0).0,
            workspace_root,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Increases on every change, so queued approvals can be re-evaluated.
    pub fn generation(&self) -> u64 {
        *self.generation.borrow()
    }

    /// Wakes when the generation changes, so waiting agent loops can
    /// re-evaluate their queued approvals.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.generation.subscribe()
    }

    fn bump_generation(&self) {
        self.generation.send_modify(|generation| *generation += 1);
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn temporary_grants(&self) -> Vec<Grant> {
        self.lock().temporary.clone()
    }

    pub fn persistent_grants(&self) -> Vec<Grant> {
        self.lock().persistent.clone()
    }

    pub fn add_temporary(&self, grant: Grant) {
        let mut inner = self.lock();
        if !inner.temporary.contains(&grant) {
            inner.temporary.push(grant);
        }
        self.bump_generation();
    }

    pub fn clear_temporary(&self) {
        let mut inner = self.lock();
        if !inner.temporary.is_empty() {
            inner.temporary.clear();
            self.bump_generation();
        }
    }

    /// Whether persistent grants can be saved, with the reason if not.
    pub fn persistence_unavailable(&self) -> Option<String> {
        let inner = self.lock();
        match &inner.store {
            None => Some("no workspace profile directory is available".to_string()),
            Some(store) => store.refusal().map(|error| error.to_string()),
        }
    }

    /// The `.ylva` directory saving would create, if any.
    pub fn marker_to_create(&self) -> Option<PathBuf> {
        self.lock()
            .store
            .as_ref()
            .and_then(GrantStore::marker_to_create)
    }

    /// Save `grant` to `grants.toml` and make it effective in this run.
    /// Saves are serialized by the state lock.
    pub fn add_persistent(&self, grant: Grant) -> Result<SaveOutcome, GrantStoreError> {
        let mut inner = self.lock();
        let store = inner.store.as_mut().ok_or_else(|| {
            GrantStoreError::Unavailable("no workspace profile directory is available".into())
        })?;
        let outcome = store.append(&grant)?;
        if !inner.persistent.contains(&grant) {
            inner.persistent.push(grant);
        }
        self.bump_generation();
        Ok(outcome)
    }

    /// Judge a call. `tool_name` selects the grants considered.
    pub fn evaluate(&self, tool_name: &str, subjects: &GrantSubjects) -> GrantDecision {
        let Some(tool) = GrantTool::from_name(tool_name) else {
            return GrantDecision::NotCovered;
        };
        let inner = self.lock();
        let find = |subject: &GrantSubject| -> Option<GrantReference> {
            inner
                .temporary
                .iter()
                .map(|grant| (grant, GrantLifetime::Temporary))
                .chain(
                    inner
                        .persistent
                        .iter()
                        .map(|grant| (grant, GrantLifetime::Persistent)),
                )
                .find(|(grant, _)| {
                    grant.tool == tool && grant.covers(subject, &self.workspace_root)
                })
                .map(|(grant, lifetime)| grant.reference(lifetime))
        };
        match subjects {
            GrantSubjects::NotGrantable => GrantDecision::NotCovered,
            GrantSubjects::Single(subject) => match find(subject) {
                Some(reference) => GrantDecision::Covered(vec![reference]),
                None => GrantDecision::NotCovered,
            },
            GrantSubjects::Commands(statements) => {
                if statements.is_empty() {
                    return GrantDecision::NotCovered;
                }
                let mut references = Vec::new();
                for statement in statements {
                    match statement {
                        CommandStatement::Unanalysable => return GrantDecision::NotCovered,
                        CommandStatement::Command {
                            policy_approved: true,
                            ..
                        } => {}
                        CommandStatement::Command { command, .. } => {
                            if command.has_unquoted_expansion
                                || command.has_variable_assignment_prefix
                            {
                                return GrantDecision::NotCovered;
                            }
                            match find(&GrantSubject::Command(command.clone())) {
                                Some(reference) => references.push(reference),
                                None => return GrantDecision::NotCovered,
                            }
                        }
                    }
                }
                GrantDecision::Covered(references)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::{CommandSubject, CommandToken};

    fn state() -> GrantState {
        GrantState::new(PathBuf::from("/work"))
    }

    fn command(words: &[&str]) -> CommandStatement {
        CommandStatement::Command {
            command: CommandSubject {
                tokens: words
                    .iter()
                    .map(|word| CommandToken::new(*word, false))
                    .collect(),
                has_unquoted_expansion: false,
                has_variable_assignment_prefix: false,
            },
            policy_approved: false,
        }
    }

    fn bash_grant(rule: &str) -> Grant {
        Grant::from_stored(GrantTool::Bash, None, Some(rule)).unwrap()
    }

    #[test]
    fn temporary_grant_covers_and_clear_revokes() {
        let state = state();
        let grant = Grant::from_stored(GrantTool::Read, None, Some("./src/*")).unwrap();
        let subjects = GrantSubjects::Single(GrantSubject::Path(PathBuf::from("/work/src/a.rs")));
        assert_eq!(state.evaluate("read", &subjects), GrantDecision::NotCovered);
        let before = state.generation();
        state.add_temporary(grant);
        assert!(state.generation() > before);
        assert!(matches!(
            state.evaluate("read", &subjects),
            GrantDecision::Covered(_)
        ));
        assert_eq!(
            state.evaluate("write", &subjects),
            GrantDecision::NotCovered
        );
        state.clear_temporary();
        assert_eq!(state.evaluate("read", &subjects), GrantDecision::NotCovered);
    }

    #[test]
    fn unconditional_and_unknown_tools() {
        let state = state();
        state.add_temporary(Grant::from_stored(GrantTool::WebSearch, None, None).unwrap());
        let subjects = GrantSubjects::Single(GrantSubject::Unconditional);
        assert!(matches!(
            state.evaluate("web_search", &subjects),
            GrantDecision::Covered(_)
        ));
        assert_eq!(
            state.evaluate("clarify", &subjects),
            GrantDecision::NotCovered
        );
        assert_eq!(
            state.evaluate("web_search", &GrantSubjects::NotGrantable),
            GrantDecision::NotCovered
        );
    }

    #[test]
    fn every_bash_statement_must_be_covered() {
        let state = state();
        state.add_temporary(bash_grant("cargo test"));
        let covered = command(&["cargo", "test"]);
        let uncovered = command(&["rm", "x"]);
        let policy_approved = CommandStatement::Command {
            command: CommandSubject {
                tokens: vec![CommandToken::new("ls", false)],
                has_unquoted_expansion: false,
                has_variable_assignment_prefix: false,
            },
            policy_approved: true,
        };
        assert!(matches!(
            state.evaluate(
                "bash",
                &GrantSubjects::Commands(vec![covered.clone(), policy_approved])
            ),
            GrantDecision::Covered(references) if references.len() == 1
        ));
        assert_eq!(
            state.evaluate(
                "bash",
                &GrantSubjects::Commands(vec![covered.clone(), uncovered])
            ),
            GrantDecision::NotCovered
        );
        assert_eq!(
            state.evaluate(
                "bash",
                &GrantSubjects::Commands(vec![covered, CommandStatement::Unanalysable])
            ),
            GrantDecision::NotCovered
        );
        assert_eq!(
            state.evaluate("bash", &GrantSubjects::Commands(Vec::new())),
            GrantDecision::NotCovered
        );
    }

    #[test]
    fn expansion_and_assignment_prefix_are_never_covered() {
        let state = state();
        state.add_temporary(bash_grant("cargo test"));
        for (expansion, assignment) in [(true, false), (false, true)] {
            let statement = CommandStatement::Command {
                command: CommandSubject {
                    tokens: vec![
                        CommandToken::new("cargo", false),
                        CommandToken::new("test", false),
                    ],
                    has_unquoted_expansion: expansion,
                    has_variable_assignment_prefix: assignment,
                },
                policy_approved: false,
            };
            assert_eq!(
                state.evaluate("bash", &GrantSubjects::Commands(vec![statement])),
                GrantDecision::NotCovered
            );
        }
    }

    #[test]
    fn persistent_without_store_is_unavailable() {
        let state = state();
        assert!(state.persistence_unavailable().is_some());
        let grant = Grant::from_stored(GrantTool::WebSearch, None, None).unwrap();
        assert!(matches!(
            state.add_persistent(grant),
            Err(GrantStoreError::Unavailable(_))
        ));
    }

    #[test]
    fn persistent_grant_is_saved_and_effective() {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let (store, grants) = GrantStore::load(&root, "default", None).unwrap();
        let state = GrantState::with_persistent(root.clone(), grants, Some(store));
        let grant = Grant::from_stored(GrantTool::WebSearch, None, None).unwrap();
        let outcome = state.add_persistent(grant).unwrap();
        assert!(outcome.path.exists());
        assert!(matches!(
            state.evaluate("web_search", &GrantSubjects::Single(GrantSubject::Unconditional)),
            GrantDecision::Covered(references) if references[0].as_str().starts_with("persistent:")
        ));
    }
}
