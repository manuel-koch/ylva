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

//! Provider-independent export orchestration.

mod destination;
mod interrupt;
mod inventory;
mod layout;
mod markdown;
mod records;
mod report;
#[cfg(test)]
mod tests;

use crate::cli::{ExportFormat, SessionExport};
use crate::exit_code::ExitCode;
use crate::store::Store;
use crate::trufflehog;
use destination::publication::{HashingWriter, PublishError, Publisher};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(super) fn run(
    options: &SessionExport,
    chats_root: &Path,
    project_key: &str,
    launch_directory: &Path,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> anyhow::Result<u8> {
    let interrupt = interrupt::Interrupt::install()?;
    let result = run_export(
        options,
        chats_root,
        project_key,
        launch_directory,
        output,
        diagnostics,
        &|| interrupt.requested(),
    );
    if interrupt.requested() {
        let _ = writeln!(diagnostics, "error: session export interrupted");
        Ok(ExitCode::Interrupted.into())
    } else {
        result
    }
}

fn run_export(
    options: &SessionExport,
    chats_root: &Path,
    project_key: &str,
    launch_directory: &Path,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
    interrupted: &impl Fn() -> bool,
) -> anyhow::Result<u8> {
    writeln!(
        diagnostics,
        "warning: exports of active sessions or subagents may be partial; \
         each transcript includes only what was saved when it was captured"
    )?;
    if options.raw {
        writeln!(
            diagnostics,
            "warning: raw export may contain secrets and other sensitive data"
        )?;
    } else {
        writeln!(
            diagnostics,
            "warning: sanitized exports can still contain sensitive data"
        )?;
    }
    let markdown = options.format == ExportFormat::Md;
    if markdown {
        writeln!(
            diagnostics,
            "warning: Markdown may contain active HTML, links, or remote images"
        )?;
    }
    let roots = inventory::distinct_roots(&options.session_ids);
    let output_home = match layout::home_for_path(options.output.as_deref()) {
        Ok(home) => home,
        Err(error) => {
            writeln!(diagnostics, "error: invalid --output path: {error}")?;
            return Ok(ExitCode::UsageError.into());
        }
    };
    let output_path = match layout::OutputPath::parse(
        options.output.as_deref(),
        &roots,
        project_key,
        options.format,
    ) {
        Ok(path) => path,
        Err(error) => {
            writeln!(diagnostics, "error: {error}")?;
            return Ok(ExitCode::UsageError.into());
        }
    };
    let manifest_path = match options
        .manifest
        .as_deref()
        .map(destination::manifest_path)
        .transpose()
    {
        Ok(path) => path,
        Err(error) => {
            writeln!(diagnostics, "error: invalid --manifest path: {error}")?;
            return Ok(ExitCode::UsageError.into());
        }
    };
    let mut excluded = vec![chats_root.to_path_buf()];
    excluded.extend(output_path.scratch_exclusions(output_home.as_deref()));
    excluded.extend(manifest_path.iter().cloned());
    let scratch = scratch_directory(&excluded)?;
    let chats = destination::Directory::resolve(chats_root)?;
    let source_home =
        destination::Directory::resolve(chats_root.parent().unwrap_or_else(|| Path::new(".")))?;
    let scanner = if options.raw {
        None
    } else {
        match trufflehog::Scanner::resolve(scratch.path(), interrupted) {
            Ok(scanner) => Some(scanner),
            Err(error) => {
                writeln!(diagnostics, "error: {error}")?;
                return Ok(ExitCode::UsageError.into());
            }
        }
    };
    let scanner_version = scanner.as_ref().map(|scanner| scanner.version.as_str());
    let recursive = !options.no_subagents;
    let inventory = inventory::Inventory::capture(
        roots,
        recursive,
        &Store::export_reader(chats_root, project_key),
        scratch.path(),
        interrupted,
    );
    if interrupted() {
        return Ok(ExitCode::Interrupted.into());
    }
    let mut layout = match output_path.select(inventory.roots.len(), inventory.sources.len()) {
        Ok(layout) => layout,
        Err(error) => {
            writeln!(diagnostics, "error: {error}")?;
            return Ok(ExitCode::UsageError.into());
        }
    };
    let mut failure = false;
    let mut processed = HashMap::new();
    let mut indexes = HashMap::new();
    for &identity in &inventory.order {
        match &inventory.sources[&identity] {
            Ok(snapshot) => {
                for issue in &snapshot.issues {
                    writeln!(
                        diagnostics,
                        "warning: session {identity}, line {}: {}",
                        issue.line,
                        issue.reason.code()
                    )?;
                    failure |= recursive && issue.reason.affects_discovery();
                }
                if scanner.is_some() || markdown {
                    match records::prepare(
                        identity,
                        snapshot,
                        scanner.as_ref(),
                        scratch.path(),
                        interrupted,
                    ) {
                        Ok(data) => {
                            failure |= data.incomplete;
                            if markdown {
                                match markdown::Index::build(snapshot, &data, interrupted) {
                                    Ok(index) => {
                                        indexes.insert(identity, index);
                                    }
                                    Err(error) => {
                                        writeln!(
                                            diagnostics,
                                            "error: could not index session {identity}: {error}"
                                        )?;
                                        failure = true;
                                    }
                                }
                            }
                            processed.insert(identity, data);
                        }
                        Err(error) => {
                            writeln!(diagnostics, "error: session {identity}: {error}")?;
                            failure = true;
                        }
                    }
                }
            }
            Err(error) => {
                writeln!(diagnostics, "error: session {identity}: {error}")?;
                failure = true;
            }
        }
        if interrupted() {
            return Ok(ExitCode::Interrupted.into());
        }
    }
    layout.resolve_paths(output_home.as_deref(), &inventory.roots, |identity| {
        if options.raw {
            inventory.snapshot(identity)?.title_slug.as_deref()
        } else {
            processed.get(&identity)?.title_slug.as_deref()
        }
    });
    let mut rendered = Vec::new();
    let mut staged = Vec::new();
    let bundles = inventory.bundles(recursive);
    for bundle in &bundles {
        let mut transcripts = Vec::new();
        let links = markdown::Links {
            bundle,
            inventory: &inventory,
            indexes: &indexes,
            bundled: matches!(layout, layout::Layout::Bundles(_)),
        };
        for &member in &bundle.members {
            if interrupted() {
                return Ok(ExitCode::Interrupted.into());
            }
            let source = if markdown {
                if !indexes.contains_key(&member.identity) {
                    continue;
                }
                if !matches!(layout, layout::Layout::Stdout) {
                    let render =
                        tempfile::NamedTempFile::new_in(scratch.path()).and_then(|mut file| {
                            let mut writer = io::BufWriter::new(file.as_file_mut());
                            markdown::render(
                                &mut writer,
                                member.identity,
                                &processed[&member.identity],
                                &links,
                                interrupted,
                            )?;
                            writer.flush()?;
                            drop(writer);
                            Ok(file.into_temp_path())
                        });
                    match render {
                        Ok(path) => {
                            let bytes = path.to_path_buf();
                            rendered.push(path);
                            Some(bytes)
                        }
                        Err(error) => {
                            writeln!(
                                diagnostics,
                                "error: could not render {}: {error}",
                                member.identity
                            )?;
                            failure = true;
                            continue;
                        }
                    }
                } else {
                    None
                }
            } else if scanner.is_some() {
                let Some(data) = processed.get(&member.identity) else {
                    continue;
                };
                Some(data.bytes.to_path_buf())
            } else {
                let Some(snapshot) = inventory.snapshot(member.identity) else {
                    continue;
                };
                Some(snapshot.bytes.to_path_buf())
            };
            transcripts.push((member, source));
        }
        staged.push(transcripts);
    }
    if interrupted() {
        return Ok(ExitCode::Interrupted.into());
    }
    if let layout::Layout::Bundles(paths) = &layout {
        let scratch_location = destination::Directory::resolve(scratch.path())?;
        for (path, transcripts) in paths.iter().zip(&staged) {
            if !transcripts.is_empty()
                && destination::Directory::resolve(path).is_ok_and(|bundle| {
                    bundle.contains(&scratch_location) || scratch_location.contains(&bundle)
                })
            {
                writeln!(
                    diagnostics,
                    "error: export bundle overlaps the private scratch directory"
                )?;
                return Ok(ExitCode::UsageError.into());
            }
        }
    }
    let plan = match destination::Plan::preflight(
        &layout,
        staged
            .iter()
            .map(|transcripts| transcripts.iter().map(|(member, _)| *member).collect()),
        layout::extension(options.format),
        &chats,
        manifest_path.as_deref(),
        options.force,
        interrupted,
    ) {
        Ok(plan) => plan,
        Err(_) if interrupted() => return Ok(ExitCode::Interrupted.into()),
        Err(error) => {
            writeln!(diagnostics, "error: destination preflight: {error}")?;
            return Ok(ExitCode::UsageError.into());
        }
    };
    let mut publisher = Publisher::new(&chats);
    let mut outcomes = report::Outcomes::default();
    for ((bundle, transcripts), destinations) in bundles.iter().zip(staged).zip(plan.bundles) {
        let output_start = outcomes.outputs.len();
        let bundle_parent = destinations
            .directory
            .as_deref()
            .filter(|_| !interrupted())
            .map(|directory| publisher.bundle(directory, interrupted))
            .transpose();
        let mut planned = transcripts
            .into_iter()
            .zip(destinations.transcripts)
            .peekable();
        for &member in &bundle.members {
            let identity = member.identity;
            let data = processed.get(&identity);
            let mut outcome = report::Output::new(
                bundle.root,
                member,
                data,
                inventory.snapshot(identity).and(scanner_version),
            );
            let Some(((_, bytes), destination)) =
                planned.next_if(|((staged, _), _)| staged.identity == identity)
            else {
                outcomes.outputs.push(outcome);
                continue;
            };
            outcome.set_target(destination.as_ref(), data);
            if interrupted() {
                outcomes.outputs.push(outcome);
                continue;
            }
            let result = match (&bundle_parent, destination, bytes) {
                (Err(error), _, _) => {
                    Err(io::Error::other(format!("could not open bundle: {error}")).into())
                }
                (Ok(parent), Some(destination), Some(bytes)) => File::open(bytes)
                    .map_err(PublishError::from)
                    .and_then(|source| match parent {
                        Some(parent) => publisher.commit(parent, &destination, source, interrupted),
                        None => publisher.standalone(&destination, source, interrupted),
                    }),
                (_, None, Some(bytes)) => File::open(bytes)
                    .and_then(|source| {
                        stream_stdout(output, interrupted, |writer| {
                            io::copy(
                                &mut interrupt::InterruptibleReader::new(source, interrupted),
                                writer,
                            )
                            .map(|_| ())
                        })
                    })
                    .map_err(PublishError::from),
                (_, None, None) => {
                    let links = markdown::Links {
                        bundle,
                        inventory: &inventory,
                        indexes: &indexes,
                        bundled: false,
                    };
                    stream_stdout(output, interrupted, |writer| {
                        markdown::render(
                            writer,
                            identity,
                            &processed[&identity],
                            &links,
                            interrupted,
                        )
                    })
                    .map_err(PublishError::from)
                }
                (_, Some(_), None) => Err(io::Error::other("missing staged transcript").into()),
            };
            outcome.finish(result);
            failure |= outcome.failed();
            outcomes.outputs.push(outcome);
        }
        outcomes.bundles.push(report::Bundle::new(
            bundle.root,
            destinations
                .directory
                .as_ref()
                .map(|directory| directory.path()),
            &outcomes.outputs[output_start..],
        ));
    }

    let manifest = plan.manifest.map(|destination| {
        let manifest = report::Manifest::new(
            source_home.path(),
            project_key,
            launch_directory,
            &inventory,
            &outcomes,
            recursive,
            report::Status::from_outcomes(failure, interrupted()),
        );
        let result = manifest.publish(&mut publisher, &destination, scratch.path(), interrupted);
        (destination.path(), result)
    });
    for outcome in &outcomes.outputs {
        outcome.report(diagnostics)?;
    }
    for path in publisher.created_paths {
        writeln!(
            diagnostics,
            "created export directory {}",
            crate::home_relative_path::quote_path(&path)
        )?;
    }
    if let Some((path, result)) = manifest {
        report::manifest_outcome(diagnostics, &path, &result)?;
        failure |= result.is_err();
    }
    Ok(report::Status::from_outcomes(failure, interrupted())
        .exit_code()
        .into())
}

fn stream_stdout(
    output: &mut dyn Write,
    interrupted: &impl Fn() -> bool,
    stream: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<String> {
    let mut writer = HashingWriter::new(output);
    let result = stream(&mut writer);
    let flush = writer.flush();
    result.and(flush)?;
    interrupt::check(interrupted)?;
    Ok(writer.digest())
}

fn scratch_directory(excluded: &[PathBuf]) -> io::Result<tempfile::TempDir> {
    let excluded: Vec<_> = excluded
        .iter()
        .filter_map(|path| destination::Directory::resolve(path).ok())
        .collect();
    for location in [
        std::env::temp_dir(),
        PathBuf::from("/var/tmp"),
        PathBuf::from("/tmp"),
    ] {
        let Ok(location) = destination::Directory::resolve(&location) else {
            continue;
        };
        if excluded.iter().any(|path| path.contains(&location)) {
            continue;
        }
        if let Ok(directory) = tempfile::Builder::new()
            .prefix("ylva-export-")
            .tempdir_in(location.path())
        {
            return Ok(directory);
        }
    }
    Err(io::Error::other(
        "no private scratch location outside the source and export trees",
    ))
}
