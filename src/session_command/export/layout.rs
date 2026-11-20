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

use crate::cli::ExportFormat;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const TITLE_TOKEN: &str = "{session_title}";
pub(super) const UNTITLED: &str = "untitled";

pub(super) enum OutputPath {
    Stdout,
    Fixed(PathBuf),
    PerRoot(Vec<PathBuf>),
}

pub(super) enum Layout {
    Stdout,
    File(PathBuf),
    Bundles(Vec<PathBuf>),
}

impl OutputPath {
    pub fn parse(
        path: Option<&Path>,
        roots: &[Uuid],
        project_key: &str,
        format: ExportFormat,
    ) -> Result<Self, &'static str> {
        let Some(path) = path else {
            return Ok(Self::Stdout);
        };
        let bytes = path.as_os_str().as_bytes();
        if bytes.is_empty() || bytes.contains(&0) {
            return Err("output must name a path");
        }
        if !bytes.iter().any(|byte| matches!(byte, b'{' | b'}')) {
            return Ok(Self::Fixed(path.to_owned()));
        }
        roots
            .iter()
            .map(|root| expand(bytes, *root, project_key, extension(format)))
            .collect::<Result<_, _>>()
            .map(Self::PerRoot)
    }

    pub fn scratch_exclusions<'a>(
        &'a self,
        home: Option<&'a Path>,
    ) -> impl Iterator<Item = PathBuf> + 'a {
        let paths = match self {
            Self::Stdout => &[],
            Self::Fixed(path) => std::slice::from_ref(path),
            Self::PerRoot(paths) => paths,
        };
        paths.iter().map(move |path| {
            let bytes = path.as_os_str().as_bytes();
            let Some(title) = title_token_offset(bytes) else {
                return resolve_home(path, home);
            };
            // Until the title is known, keep scratch outside the entire
            // subtree in which the first title-dependent component can land.
            let parent = bytes[..title]
                .iter()
                .rposition(|&byte| byte == b'/')
                .map_or(&b"."[..], |index| &bytes[..=index]);
            resolve_home(Path::new(std::ffi::OsStr::from_bytes(parent)), home)
        })
    }

    pub fn select(self, roots: usize, included: usize) -> Result<Layout, &'static str> {
        Ok(match self {
            Self::Stdout if included == 1 => Layout::Stdout,
            Self::Stdout => return Err("multiple session IDs require --output"),
            Self::Fixed(path) if included == 1 => Layout::File(path),
            Self::Fixed(path) if roots == 1 => Layout::Bundles(vec![path]),
            Self::Fixed(path) => Layout::Bundles(
                (1..=roots)
                    .map(|number| path.join(format!("session-{number}")))
                    .collect(),
            ),
            Self::PerRoot(paths) => Layout::Bundles(paths),
        })
    }
}

impl Layout {
    pub fn resolve_paths<'a>(
        &mut self,
        home: Option<&Path>,
        roots: &[Uuid],
        title_slug: impl Fn(Uuid) -> Option<&'a str>,
    ) {
        let paths = match self {
            Self::Stdout => return,
            Self::File(path) => std::slice::from_mut(path),
            Self::Bundles(paths) => paths.as_mut_slice(),
        };
        for (path, &root) in paths.iter_mut().zip(roots) {
            let mut bytes = path.as_os_str().as_bytes();
            if title_token_offset(bytes).is_none() {
                if home.is_some() {
                    *path = resolve_home(path, home);
                }
                continue;
            }
            let slug = title_slug(root).unwrap_or(UNTITLED);
            let mut expanded = Vec::new();
            while let Some(index) = title_token_offset(bytes) {
                expanded.extend_from_slice(&bytes[..index]);
                expanded.extend_from_slice(slug.as_bytes());
                bytes = &bytes[index + TITLE_TOKEN.len()..];
            }
            expanded.extend_from_slice(bytes);
            *path = resolve_home(Path::new(&OsString::from_vec(expanded)), home);
        }
    }
}

pub(super) fn home_for_path(path: Option<&Path>) -> Result<Option<PathBuf>, &'static str> {
    if !path.is_some_and(|path| path.starts_with("~")) {
        return Ok(None);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(Some)
        .ok_or("a home-relative path requires an absolute HOME")
}

pub(super) fn resolve_home(path: &Path, home: Option<&Path>) -> PathBuf {
    match (home, path.strip_prefix("~")) {
        (Some(home), Ok(relative)) => home.join(relative),
        _ => path.to_owned(),
    }
}

fn title_token_offset(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(TITLE_TOKEN.len())
        .position(|window| window == TITLE_TOKEN.as_bytes())
}

pub(super) fn title_slug(title: &str) -> Option<String> {
    const MAX_LENGTH: usize = 64;
    let mut slug = String::new();
    let mut separator = false;
    for character in title.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                if slug.len() + 1 == MAX_LENGTH {
                    break;
                }
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            if slug.len() == MAX_LENGTH {
                break;
            }
            separator = false;
        } else {
            separator = true;
        }
    }
    (!slug.is_empty()).then_some(slug)
}

pub(super) fn extension(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Md => "md",
        ExportFormat::Jsonl => "jsonl",
    }
}

fn expand(
    mut bytes: &[u8],
    root: Uuid,
    project_key: &str,
    format: &str,
) -> Result<PathBuf, &'static str> {
    const INVALID: &str =
        "invalid output template; use {project_key}, {session_id}, {session_title}, or {format}";
    let mut expanded = Vec::with_capacity(bytes.len());
    while let Some(index) = bytes.iter().position(|byte| matches!(byte, b'{' | b'}')) {
        expanded.extend_from_slice(&bytes[..index]);
        if bytes[index] != b'{' {
            return Err(INVALID);
        }
        let after_open = &bytes[index + 1..];
        let end = after_open
            .iter()
            .position(|byte| *byte == b'}')
            .ok_or(INVALID)?;
        let token = &after_open[..end];
        match token {
            b"project_key" => expanded.extend_from_slice(project_key.as_bytes()),
            b"session_id" => expanded.extend_from_slice(root.to_string().as_bytes()),
            b"session_title" => expanded.extend_from_slice(TITLE_TOKEN.as_bytes()),
            b"format" => expanded.extend_from_slice(format.as_bytes()),
            _ => return Err(INVALID),
        }
        bytes = &after_open[end + 1..];
    }
    expanded.extend_from_slice(bytes);
    Ok(PathBuf::from(OsString::from_vec(expanded)))
}
