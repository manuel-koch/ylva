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

//! Deterministic, bounded Agent Skill discovery. Discovery never grants permissions.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use sha2::{Digest, Sha256};

mod fingerprint;
pub mod policy;

/// Exact-case filename identifying an Agent Skill's instruction file.
pub const SKILL_FILE_NAME: &str = "SKILL.md";

pub const MAX_SKILL_BYTES: usize = 64 * 1024;
pub const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SKILLS: usize = 128;

#[derive(Debug, Clone)]
pub struct SkillSource {
    /// Discovered instruction-file path, preserving any logical symlink aliases.
    pub logical_path: PathBuf,
    /// Absolute canonical instruction-file identity captured during discovery.
    /// Its parent anchors relative resources and source-based sandbox checks.
    pub canonical_path: PathBuf,
    /// Original search root containing the logical skill directory, retained
    /// for precedence provenance rather than resolved to its canonical target.
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub source: SkillSource,
    pub shadowed: Vec<SkillSource>,
    pub duplicates: Vec<SkillSource>,
    pub raw_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillDiagnosticKind {
    Io,
    Invalid,
    FileLimit,
    CatalogLimit,
    Comparison,
}

#[derive(Debug, Clone)]
pub struct SkillDiagnostic {
    pub path: PathBuf,
    pub name: Option<String>,
    pub kind: SkillDiagnosticKind,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct SkillCatalog {
    pub skills: BTreeMap<String, Arc<Skill>>,
    pub diagnostics: Vec<SkillDiagnostic>,
}

impl SkillCatalog {
    /// The eight supported roots, in precedence order. No environment is consulted.
    pub fn discover(
        workspace_root: Option<&Path>,
        launch_directory: &Path,
        os_home: Option<&Path>,
        ylva_home: Option<&Path>,
    ) -> std::io::Result<Self> {
        if !launch_directory.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "launch directory is unavailable",
            ));
        }
        let local = workspace_root.unwrap_or(launch_directory);
        let mut roots: Vec<_> = [".ylva", ".agents", ".github", ".claude"]
            .iter()
            .map(|directory| local.join(directory).join("skills"))
            .collect();
        if let Some(home) = ylva_home {
            roots.push(home.join("skills"));
        }
        if let Some(home) = os_home {
            roots.extend(
                [".agents", ".copilot", ".claude"]
                    .iter()
                    .map(|directory| home.join(directory).join("skills")),
            );
        }
        Ok(Self::from_roots(&roots))
    }

    fn diagnostic(
        &mut self,
        path: &Path,
        name: Option<String>,
        kind: SkillDiagnosticKind,
        reason: impl ToString,
    ) {
        self.diagnostics.push(SkillDiagnostic {
            path: path.to_owned(),
            name,
            kind,
            reason: reason.to_string(),
        });
    }

    fn from_roots(roots: &[PathBuf]) -> Self {
        let mut catalog = Self::default();
        let mut identities = HashMap::<PathBuf, String>::new();
        let mut instruction_hashes = HashMap::new();
        let mut fingerprints = HashMap::new();
        let mut retained_bytes = 0;
        for root in roots {
            let entries = match std::fs::read_dir(root) {
                Ok(entries) => entries,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && std::fs::symlink_metadata(root).is_err() =>
                {
                    continue;
                }
                Err(error) => {
                    catalog.diagnostic(root, None, SkillDiagnosticKind::Io, error);
                    continue;
                }
            };
            let mut directories = Vec::new();
            for entry in entries {
                match entry {
                    Ok(entry) => directories.push(entry.path()),
                    Err(error) => {
                        catalog.diagnostic(root, None, SkillDiagnosticKind::Io, error);
                    }
                }
            }
            directories.sort();
            for directory in directories {
                match std::fs::metadata(&directory) {
                    Ok(metadata) if metadata.is_dir() => {}
                    Ok(_) => continue,
                    Err(error) => {
                        catalog.diagnostic(&directory, None, SkillDiagnosticKind::Io, error);
                        continue;
                    }
                }
                // Enumerating names is intentional: exists() is case-insensitive on macOS.
                let files = match std::fs::read_dir(&directory) {
                    Ok(files) => files,
                    Err(error) => {
                        catalog.diagnostic(&directory, None, SkillDiagnosticKind::Io, error);
                        continue;
                    }
                };
                let mut instruction_path = None;
                for file in files {
                    match file {
                        Ok(file) if file.file_name() == SKILL_FILE_NAME => {
                            instruction_path = Some(file.path());
                        }
                        Ok(_) => {}
                        Err(error) => {
                            catalog.diagnostic(&directory, None, SkillDiagnosticKind::Io, error);
                        }
                    }
                }
                let Some(path) = instruction_path else {
                    continue;
                };
                let logical_name = directory
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default();
                let source = match std::fs::canonicalize(&path) {
                    Ok(canonical_path) => SkillSource {
                        logical_path: path.clone(),
                        canonical_path,
                        root: root.clone(),
                    },
                    Err(error) => {
                        catalog.diagnostic(
                            &path,
                            Some(logical_name.to_owned()),
                            SkillDiagnosticKind::Io,
                            error,
                        );
                        continue;
                    }
                };
                if let Some(name) = identities.get(&source.canonical_path) {
                    if let Some(skill) = catalog.skills.get_mut(name) {
                        Arc::make_mut(skill).duplicates.push(source);
                    }
                    continue;
                }
                let (skill, instruction_hash) = match read_skill(logical_name, source) {
                    Ok(skill) => skill,
                    Err((kind, reason)) => {
                        catalog.diagnostic(&path, Some(logical_name.to_owned()), kind, reason);
                        continue;
                    }
                };
                if let Some(winner) = catalog.skills.get(&skill.name).cloned() {
                    let winner_hash = instruction_hashes[&skill.name];
                    let identical = if instruction_hash == winner_hash {
                        let preferred =
                            catalog.fingerprint(&winner, &winner_hash, &mut fingerprints);
                        let candidate =
                            catalog.fingerprint(&skill, &instruction_hash, &mut fingerprints);
                        preferred.is_some() && preferred == candidate
                    } else {
                        false
                    };
                    drop(winner);
                    let winner = Arc::make_mut(catalog.skills.get_mut(&skill.name).unwrap());
                    if identical {
                        winner.duplicates.push(skill.source);
                    } else {
                        winner.shadowed.push(skill.source);
                    }
                    continue;
                }
                if catalog.skills.len() >= MAX_SKILLS
                    || retained_bytes + skill.raw_bytes > MAX_CATALOG_BYTES
                {
                    catalog.diagnostic(
                        &path,
                        Some(skill.name),
                        SkillDiagnosticKind::CatalogLimit,
                        format!(
                            "winning catalog exceeds {MAX_SKILLS} names or {MAX_CATALOG_BYTES} raw bytes"
                        ),
                    );
                    continue;
                }
                retained_bytes += skill.raw_bytes;
                identities.insert(skill.source.canonical_path.clone(), skill.name.clone());
                instruction_hashes.insert(skill.name.clone(), instruction_hash);
                catalog.skills.insert(skill.name.clone(), Arc::new(skill));
            }
        }
        catalog
    }

    fn fingerprint(
        &mut self,
        skill: &Skill,
        instruction_hash: &[u8; 32],
        cache: &mut HashMap<PathBuf, Option<[u8; 32]>>,
    ) -> Option<[u8; 32]> {
        *cache
            .entry(skill.source.canonical_path.clone())
            .or_insert_with(|| {
                match fingerprint::calculate(&skill.source.canonical_path, instruction_hash) {
                    Ok(fingerprint) => Some(fingerprint),
                    Err(error) => {
                        self.diagnostic(
                            &skill.source.logical_path,
                            Some(skill.name.clone()),
                            SkillDiagnosticKind::Comparison,
                            format!("duplicate comparison unavailable: {error}"),
                        );
                        None
                    }
                }
            })
    }
}

fn read_skill(
    logical_name: &str,
    source: SkillSource,
) -> Result<(Skill, [u8; 32]), (SkillDiagnosticKind, String)> {
    let read = || -> std::io::Result<Vec<u8>> {
        let file = std::fs::File::open(&source.canonical_path)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "skill source is not a regular file",
            ));
        }
        let mut bytes = Vec::new();
        file.take((MAX_SKILL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let bytes = read().map_err(|error| (SkillDiagnosticKind::Io, error.to_string()))?;
    if bytes.len() > MAX_SKILL_BYTES {
        return Err((
            SkillDiagnosticKind::FileLimit,
            format!("skill exceeds {MAX_SKILL_BYTES} raw bytes"),
        ));
    }
    let invalid = |reason: String| (SkillDiagnosticKind::Invalid, reason);
    let contents = std::str::from_utf8(&bytes).map_err(|error| invalid(error.to_string()))?;
    let (name, description, body) = parse_skill(contents, logical_name).map_err(invalid)?;
    let instruction_hash = Sha256::digest(&bytes).into();
    Ok((
        Skill {
            name,
            description,
            body,
            source,
            shadowed: Vec::new(),
            duplicates: Vec::new(),
            raw_bytes: bytes.len(),
        },
        instruction_hash,
    ))
}

fn parse_skill(contents: &str, directory_name: &str) -> Result<(String, String, String), String> {
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    let mut lines = contents.split_inclusive('\n');
    if lines.next().map(|line| line.trim_end_matches(['\r', '\n'])) != Some("---") {
        return Err("skill must start with YAML frontmatter ('---')".into());
    }
    let mut frontmatter = String::new();
    let mut body = None;
    for line in lines.by_ref() {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            body = Some(lines.collect::<String>());
            break;
        }
        frontmatter.push_str(line);
    }
    let body = body.ok_or("skill frontmatter is missing its closing '---'")?;
    // The YAML parser rejects duplicate keys, including unknown fields, and (with
    // `no_schema`) refuses to coerce unquoted numbers, booleans and nulls into
    // strings, so `name`/`description` must be real strings.
    let metadata: SkillFrontmatter =
        serde_saphyr::from_str_with_options(&frontmatter, crate::yaml_options())
            .map_err(|error| format!("invalid YAML: {error}"))?;
    let name = metadata.name.as_str();
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || name != directory_name
    {
        return Err("name must match its directory and contain 1-64 lowercase ASCII letters/digits/hyphens, without edge or consecutive hyphens".into());
    }
    let description = metadata.description.as_str();
    if description.trim().is_empty() || description.chars().count() > 1024 {
        return Err("description must be nonblank and at most 1024 characters".into());
    }
    if body.trim().is_empty() {
        return Err("skill must have a nonblank body".into());
    }
    Ok((name.into(), description.into(), body))
}

/// Required frontmatter fields; unknown fields are ignored.
#[derive(Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
}

/// Safe single-line presentation for untrusted metadata and diagnostic paths.
pub fn terminal_safe(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                character.escape_default().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
