//! Provider-independent session command orchestration.

use crate::cli::SessionCommand;
use crate::exit_code::ExitCode;
use crate::store::{
    default_chats_root, project_key, MaintenanceAction, SessionSelection, SessionSummary, Store,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

#[cfg(test)]
mod tests;

pub fn parse_date(value: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    if let Ok(date) = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        if date.format("%Y-%m-%d").to_string() == value {
            return Ok(date.and_hms_opt(0, 0, 0).expect("midnight").and_utc());
        }
    }
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|date| date.with_timezone(&chrono::Utc))
        .map_err(|error| {
            format!("invalid date '{value}': expected YYYY-MM-DD or RFC 3339 with offset: {error}")
        })
}

pub fn run(
    command: &SessionCommand,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> anyhow::Result<u8> {
    let current_directory = std::env::current_dir()?.canonicalize()?;
    let chats_root = default_chats_root()?;
    let project_key = project_key(&current_directory);
    tracing::debug!(
        cwd = %current_directory.display(),
        chats_root = %chats_root.display(),
        project_key = %project_key,
        "resolved session store"
    );
    run_for_project(command, &chats_root, &project_key, output, diagnostics)
}

fn write_session_line(
    session: &SessionSummary,
    depth: usize,
    annotation: &str,
    output: &mut dyn Write,
) -> std::io::Result<()> {
    let marker = if depth == 0 {
        String::new()
    } else {
        format!("{}+ ", "  ".repeat(depth - 1))
    };
    writeln!(
        output,
        "{} {}{} {}{}",
        session.last_used_at,
        marker,
        session.session_id,
        crate::app::single_line(&session.title),
        annotation
    )
}

fn write_session_tree(
    sessions: &[SessionSummary],
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> std::io::Result<()> {
    let identities: BTreeSet<_> = sessions.iter().map(|session| session.session_id).collect();
    let mut children = BTreeMap::<_, Vec<_>>::new();
    for session in sessions {
        if let Some(parent) = session.parent_session {
            children.entry(parent).or_default().push(session);
        }
    }
    let roots = sessions.iter().filter(|session| {
        session
            .parent_session
            .is_none_or(|parent| !identities.contains(&parent))
    });
    let mut visited = BTreeSet::new();
    // Remaining entries cover malformed parent cycles without hiding transcripts.
    for root in roots.chain(sessions.iter()) {
        if visited.contains(&root.session_id) {
            continue;
        }
        let annotation = match root.parent_session {
            Some(parent) if identities.contains(&parent) => {
                let warning = format!(
                    "session '{}' has no available hierarchy root; parent '{parent}'",
                    root.session_id
                );
                writeln!(diagnostics, "warning: {warning}")?;
                tracing::warn!(%warning, "unresolved session hierarchy");
                format!(" [unresolved parent: {parent}]")
            }
            Some(parent) => format!(" [parent unavailable: {parent}]"),
            None => String::new(),
        };
        let mut pending = vec![(root, usize::from(root.parent_session.is_some()))];
        while let Some((session, depth)) = pending.pop() {
            if !visited.insert(session.session_id) {
                continue;
            }
            write_session_line(
                session,
                depth,
                if session.session_id == root.session_id {
                    &annotation
                } else {
                    ""
                },
                output,
            )?;
            if let Some(descendants) = children.get(&session.session_id) {
                pending.extend(descendants.iter().rev().map(|child| (*child, depth + 1)));
            }
        }
    }
    Ok(())
}

fn run_for_project(
    command: &SessionCommand,
    chats_root: &std::path::Path,
    project_key: &str,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> anyhow::Result<u8> {
    match command {
        SessionCommand::List { include_subagents } => {
            let (mut sessions, warnings) =
                Store::list_sessions_with_diagnostics(chats_root, project_key)?;
            for warning in warnings {
                writeln!(diagnostics, "warning: {warning}")?;
                tracing::warn!(%warning, "unreadable session");
            }
            sessions.reverse();
            if *include_subagents {
                write_session_tree(&sessions, output, diagnostics)?;
            } else {
                for session in sessions
                    .iter()
                    .filter(|session| session.parent_session.is_none())
                {
                    write_session_line(session, 0, "", output)?;
                }
            }

            Ok(0)
        }
        SessionCommand::Delete(selector)
        | SessionCommand::Compress(selector)
        | SessionCommand::Decompress(selector) => {
            let selection = if let Some(before) = &selector.before {
                SessionSelection::Before(parse_date(before).map_err(anyhow::Error::msg)?)
            } else if selector.all {
                SessionSelection::All
            } else {
                SessionSelection::Ids(selector.session_ids.clone())
            };
            let action = match command {
                SessionCommand::Delete(_) => MaintenanceAction::Delete,
                SessionCommand::Compress(_) => MaintenanceAction::Compress,
                _ => MaintenanceAction::Decompress,
            };
            let report = Store::maintain_sessions(
                chats_root,
                project_key,
                &selection,
                action,
                selector.dry_run,
            )?;
            for notice in report.notices {
                writeln!(output, "{notice}")?;
            }
            for failure in &report.failures {
                writeln!(diagnostics, "error: {failure}")?;
                tracing::error!(%failure, "session maintenance failed");
            }
            if report.failures.is_empty() {
                Ok(u8::from(ExitCode::Success))
            } else {
                Ok(u8::from(ExitCode::GeneralError))
            }
        }
    }
}
