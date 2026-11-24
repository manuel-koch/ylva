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

//! What a tool call looks like to the grant evaluator.

use std::path::PathBuf;

/// One argv token of a simple command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandToken {
    /// The dequoted text.
    pub text: String,
    /// Whether the whole token was quoted in the source.
    pub quoted: bool,
}

impl CommandToken {
    pub fn new(text: impl Into<String>, quoted: bool) -> Self {
        CommandToken {
            text: text.into(),
            quoted,
        }
    }
}

/// An analysable simple command of a `bash` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSubject {
    /// Program followed by its arguments.
    pub tokens: Vec<CommandToken>,
    /// The command has an unquoted glob, brace expansion or `~`. No grant
    /// covers it.
    pub has_unquoted_expansion: bool,
    /// The command starts with `NAME=value` assignments. No grant covers it.
    pub has_variable_assignment_prefix: bool,
}

/// One top-level statement of a `bash` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandStatement {
    /// A simple command and whether the sandbox policy already approves it.
    Command {
        command: CommandSubject,
        policy_approved: bool,
    },
    /// A construct that can only be approved once (subshell, heredoc,
    /// substitution, control flow, non-harmless redirect).
    Unanalysable,
}

/// The single value a grant is matched against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantSubject {
    /// Resolved (absolute, canonical as far as it exists) path.
    Path(PathBuf),
    Skill {
        name: String,
        source: PathBuf,
    },
    Command(CommandSubject),
    /// `scheme://host:port` of the initial URL.
    Origin(String),
    /// A tool whose grants carry no matcher.
    Unconditional,
}

/// How a tool call is offered to the grant evaluator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantSubjects {
    /// The call cannot be covered by a grant.
    NotGrantable,
    /// One subject for the whole call.
    Single(GrantSubject),
    /// A `bash` call: each statement is judged separately.
    Commands(Vec<CommandStatement>),
}
