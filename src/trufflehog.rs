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

//! Offline TruffleHog invocation and finding mapping.

use crate::strict_json;
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs::{self, File, Metadata};
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::ops::Range;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TRUFFLEHOG_STDIN_ARGUMENTS: &[&str] = &[
    "stdin",
    "--json",
    "--no-verification",
    "--no-update",
    "--no-ignore-tag",
    "--fail-on-scan-errors",
];
const DEADLINE: Duration = Duration::from_secs(30);
const MAX_PROBE_OUTPUT: u64 = 128 * 1024;
const REDACTED: &str = "[REDACTED]";
const CREDENTIAL_NAMES: &str = r"(?:password|passwd|passphrase|api[-_]?key|api[-_]?token|access[-_]?token|refresh[-_]?token|authorization|client[-_]?secret|secret[-_]?key)";

#[derive(Debug, thiserror::Error)]
pub(crate) enum ScannerError {
    #[error("no trusted TruffleHog executable found on an absolute PATH entry")]
    Missing,
    #[error("TruffleHog does not support the required offline JSON stdin interface")]
    Incompatible,
    #[error("the selected TruffleHog executable changed")]
    Changed,
    #[error("the scanner process failed")]
    Process,
    #[error("the scanner returned invalid JSON finding data")]
    Protocol,
    #[error("the scanner exceeded its deadline")]
    Timeout,
    #[error("scanner interrupted")]
    Interrupted,
}

pub(crate) struct Scanner {
    executable: PathBuf,
    identity: ExecutableIdentity,
    pub version: String,
    assignments: Regex,
    credential_names: Regex,
}

#[derive(PartialEq, Eq)]
struct ExecutableIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl ExecutableIdentity {
    fn read(path: &Path) -> Option<Self> {
        let metadata = fs::symlink_metadata(path).ok()?;
        (metadata.is_file() && metadata.mode() & 0o111 != 0 && metadata.mode() & 0o022 == 0)
            .then(|| Self::from(metadata))
    }
}

impl From<Metadata> for ExecutableIdentity {
    fn from(metadata: Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            size: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

pub(crate) enum Findings {
    Mapped(BTreeSet<String>),
    Unmapped,
}

#[derive(Default)]
pub(crate) struct ScanText {
    content: String,
    fields: Vec<Range<usize>>,
}

impl ScanText {
    pub fn record(&mut self, value: &Value) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    self.field(key);
                    if let Some(value) = value.as_str() {
                        self.content.push_str(" = ");
                        self.field(value);
                    } else {
                        self.content.push('\n');
                        self.record(value);
                    }
                    self.content.push_str("\n\n");
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.record(value);
                }
            }
            Value::String(value) => {
                self.field(value);
                self.content.push_str("\n\n");
            }
            _ => {}
        }
    }

    fn field(&mut self, text: &str) {
        let start = self.content.len();
        self.content.push_str(text);
        self.fields.push(start..self.content.len());
    }

    fn maps(&self, value: &str) -> bool {
        let mut found = false;
        // A match in a separator or across fields would not identify a safe
        // replacement, even if a separate literal occurrence also exists.
        for (start, _) in self.content.match_indices(value) {
            let index = self.fields.partition_point(|field| field.end <= start);
            if !self
                .fields
                .get(index)
                .is_some_and(|field| field.start <= start && start + value.len() <= field.end)
            {
                return false;
            }
            found = true;
        }
        found
    }
}

impl Scanner {
    pub fn resolve(scratch: &Path, interrupted: &impl Fn() -> bool) -> Result<Self, ScannerError> {
        let search_path = std::env::var_os("PATH").ok_or(ScannerError::Missing)?;
        let (executable, identity) = std::env::split_paths(&search_path)
            .filter(|directory| directory.is_absolute())
            .filter_map(|directory| directory.join("trufflehog").canonicalize().ok())
            .find_map(|path| ExecutableIdentity::read(&path).map(|id| (path, id)))
            .ok_or(ScannerError::Missing)?;
        let assignments = Regex::new(&format!(
            r#"(?ix)(?P<prefix>\b(?:[a-z0-9]+[-_])*{CREDENTIAL_NAMES}\b["']?[\x20\t]*[:=][\x20\t]*(?:(?:bearer|basic)[\x20\t]+)?)(?P<value>"(?:\\.|[^"\\\r\n])*"?|'(?:\\.|[^'\\\r\n])*'?|[^\s,;}}"']+)"#
        )).map_err(|_| ScannerError::Incompatible)?;
        let credential_names = Regex::new(&format!(r"(?i)^(?:[a-z0-9]+[-_])*{CREDENTIAL_NAMES}$"))
            .map_err(|_| ScannerError::Incompatible)?;
        let mut scanner = Self {
            executable,
            identity,
            version: String::new(),
            assignments,
            credential_names,
        };
        let version = scanner.probe(&["--version"], scratch, interrupted)?;
        scanner.version = parse_version(&version)
            .ok_or(ScannerError::Incompatible)?
            .to_owned();
        let help = scanner
            .probe(&["stdin", "--help"], scratch, interrupted)?
            .replace("[no-]", "");
        if !TRUFFLEHOG_STDIN_ARGUMENTS
            .iter()
            .all(|argument| help.split_whitespace().any(|word| word == *argument))
        {
            return Err(ScannerError::Incompatible);
        }
        Ok(scanner)
    }

    fn probe(
        &self,
        arguments: &[&str],
        scratch: &Path,
        interrupted: &impl Fn() -> bool,
    ) -> Result<String, ScannerError> {
        let output = self
            .invoke(arguments, Stdio::null(), scratch, DEADLINE, interrupted)
            .map_err(|error| match error {
                ScannerError::Interrupted => error,
                _ => ScannerError::Incompatible,
            })?;
        let mut text = String::new();
        output
            .take(MAX_PROBE_OUTPUT)
            .read_to_string(&mut text)
            .map_err(|_| ScannerError::Incompatible)?;
        Ok(text)
    }

    fn invoke(
        &self,
        arguments: &[&str],
        stdin: Stdio,
        scratch: &Path,
        deadline: Duration,
        interrupted: &impl Fn() -> bool,
    ) -> Result<File, ScannerError> {
        if interrupted() {
            return Err(ScannerError::Interrupted);
        }
        if ExecutableIdentity::read(&self.executable).as_ref() != Some(&self.identity) {
            return Err(ScannerError::Changed);
        }
        let mut output = tempfile::tempfile_in(scratch).map_err(|_| ScannerError::Process)?;
        let stdout = output.try_clone().map_err(|_| ScannerError::Process)?;
        let child = Command::new(&self.executable)
            .args(arguments)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", scratch)
            .env("TMPDIR", scratch)
            .current_dir(scratch)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|_| ScannerError::Process)?;
        RunningScanner(Some(child)).wait(deadline, interrupted)?;
        if ExecutableIdentity::read(&self.executable).as_ref() != Some(&self.identity) {
            return Err(ScannerError::Changed);
        }
        output.rewind().map_err(|_| ScannerError::Process)?;
        Ok(output)
    }

    pub fn scan(
        &self,
        input: &ScanText,
        scratch: &Path,
        interrupted: &impl Fn() -> bool,
    ) -> Result<Findings, ScannerError> {
        if input.content.is_empty() {
            return Ok(Findings::Mapped(BTreeSet::new()));
        }
        let mut pipe_input = tempfile::tempfile_in(scratch).map_err(|_| ScannerError::Process)?;
        pipe_input
            .write_all(input.content.as_bytes())
            .map_err(|_| ScannerError::Process)?;
        pipe_input.rewind().map_err(|_| ScannerError::Process)?;
        let result = self.invoke(
            TRUFFLEHOG_STDIN_ARGUMENTS,
            pipe_input.into(),
            scratch,
            DEADLINE,
            interrupted,
        )?;
        let mut reader = BufReader::new(result);
        let mut line = Vec::new();
        let mut secrets = BTreeSet::new();
        let mut unmapped = false;
        loop {
            if interrupted() {
                return Err(ScannerError::Interrupted);
            }
            line.clear();
            if reader
                .read_until(b'\n', &mut line)
                .map_err(|_| ScannerError::Protocol)?
                == 0
            {
                break;
            }
            let finding = strict_json::object(&line).map_err(|_| ScannerError::Protocol)?;
            unmapped |= !map_finding(finding, input, &mut secrets)?;
        }
        Ok(if unmapped {
            Findings::Unmapped
        } else {
            Findings::Mapped(secrets)
        })
    }

    pub fn redact(&self, value: Value, secrets: &BTreeSet<String>) -> Result<Value, ()> {
        match value {
            Value::Object(fields) => {
                let mut redacted = Map::with_capacity(fields.len());
                for (key, value) in fields {
                    let value = if self.credential_names.is_match(&key) {
                        Value::from(REDACTED)
                    } else {
                        self.redact(value, secrets)?
                    };
                    let key = self.redact_text(key, secrets);
                    if redacted.insert(key, value).is_some() {
                        return Err(());
                    }
                }
                Ok(Value::Object(redacted))
            }
            Value::Array(values) => values
                .into_iter()
                .map(|value| self.redact(value, secrets))
                .collect(),
            Value::String(text) => Ok(Value::from(self.redact_text(text, secrets))),
            other => Ok(other),
        }
    }

    fn redact_text(&self, mut text: String, secrets: &BTreeSet<String>) -> String {
        let mut ranges: Vec<_> = secrets
            .iter()
            .flat_map(|secret| {
                text.match_indices(secret)
                    .map(|(start, _)| start..start + secret.len())
            })
            .collect();
        ranges.sort_unstable_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::new();
        for range in ranges {
            if let Some(last) = merged.last_mut().filter(|last| range.start < last.end) {
                last.end = last.end.max(range.end);
            } else {
                merged.push(range);
            }
        }
        for range in merged.into_iter().rev() {
            text.replace_range(range, REDACTED);
        }
        match self
            .assignments
            .replace_all(&text, |captures: &regex::Captures<'_>| {
                let prefix = &captures["prefix"];
                match captures["value"].as_bytes()[0] {
                    b'\'' => format!("{prefix}'{REDACTED}'"),
                    b'"' => format!("{prefix}\"{REDACTED}\""),
                    _ => format!("{prefix}{REDACTED}"),
                }
            }) {
            std::borrow::Cow::Owned(redacted) => redacted,
            std::borrow::Cow::Borrowed(_) => text,
        }
    }
}

fn parse_version(text: &str) -> Option<&str> {
    let mut parts = text.split_whitespace();
    let first = parts.next()?;
    let version = if first.eq_ignore_ascii_case("trufflehog") {
        parts.next()?
    } else {
        first
    };
    let version = version.strip_prefix('v').unwrap_or(version);
    if parts.next().is_some() || version.len() > 24 {
        return None;
    }
    let components: Vec<_> = version.split('.').collect();
    (components.len() == 3
        && components
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())))
    .then_some(version)
}

fn map_finding(
    finding: Map<String, Value>,
    input: &ScanText,
    secrets: &mut BTreeSet<String>,
) -> Result<bool, ScannerError> {
    if finding
        .get("Raw")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
        || finding
            .get("RawV2")
            .is_some_and(|value| !value.is_string() && !value.is_null())
        || finding
            .get("DecoderName")
            .is_some_and(|value| !value.is_string())
    {
        return Err(ScannerError::Protocol);
    }
    let mut mapped = matches!(
        finding.get("DecoderName").and_then(Value::as_str),
        Some("PLAIN" | "UTF8")
    );
    for (name, value) in finding {
        if !matches!(
            name.as_str(),
            "SourceMetadata"
                | "SourceID"
                | "SourceType"
                | "SourceName"
                | "DetectorType"
                | "DetectorName"
                | "DetectorDescription"
                | "DecoderName"
                | "Verified"
                | "VerificationFromCache"
        ) {
            mapped &= map_secret_values(value, input, secrets);
        }
    }
    Ok(mapped)
}

fn map_secret_values(value: Value, input: &ScanText, secrets: &mut BTreeSet<String>) -> bool {
    match value {
        Value::String(value) if !value.is_empty() => {
            let mapped = input.maps(&value);
            if mapped {
                secrets.insert(value);
            }
            mapped
        }
        Value::Array(values) => values
            .into_iter()
            .all(|value| map_secret_values(value, input, secrets)),
        Value::Object(fields) => fields
            .into_values()
            .all(|value| map_secret_values(value, input, secrets)),
        Value::Null | Value::Bool(_) | Value::String(_) => true,
        Value::Number(_) => false,
    }
}

struct RunningScanner(Option<Child>);

impl RunningScanner {
    fn wait(
        mut self,
        deadline: Duration,
        interrupted: &impl Fn() -> bool,
    ) -> Result<(), ScannerError> {
        let start = Instant::now();
        while let Some(child) = &mut self.0 {
            if interrupted() {
                return Err(ScannerError::Interrupted);
            }
            if let Some(status) = child.try_wait().map_err(|_| ScannerError::Process)? {
                self.0 = None;
                return if status.success() {
                    Ok(())
                } else {
                    Err(ScannerError::Process)
                };
            }
            if start.elapsed() >= deadline {
                return Err(ScannerError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(ScannerError::Process)
    }
}

impl Drop for RunningScanner {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            if let Ok(pid) = i32::try_from(child.id()) {
                let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_job_kills_and_reaps_a_process_that_outlives_its_deadline() {
        let job = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = Pid::from_raw(i32::try_from(job.id()).unwrap());
        assert!(matches!(
            RunningScanner(Some(job)).wait(Duration::from_millis(20), &|| false),
            Err(ScannerError::Timeout)
        ));
        assert_eq!(
            nix::sys::signal::kill(pid, None),
            Err(nix::errno::Errno::ESRCH)
        );
    }
}
