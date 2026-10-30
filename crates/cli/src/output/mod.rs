//! Human presentation only. Execution and machine-readable response values stay independent.
mod collections;
mod details;

use crate::args::{Args, Command};
use serde_json::Value;
use std::fmt::Write;

pub(crate) fn render(args: &Args, value: &Value) -> String {
    if matches!(args.command, Command::Api { .. }) {
        return serde_json::to_string_pretty(value).expect("JSON value");
    }
    let mut output = String::new();
    if !collections::render(&mut output, args, value) {
        details::render(&mut output, args, value);
    }
    output.trim_end().to_owned()
}

/// Shared evidence-only progress view; changing receipt timestamps alone is not progress.
pub(crate) fn observation(value: &Value) -> String {
    let mut output = String::new();
    fields(
        &mut output,
        value,
        &[
            ("Purpose", "/purpose"),
            ("State", "/state"),
            ("Phase", "/phase"),
            ("Known blockage", "/blocked_reason"),
            ("Observation freshness", "/observation_freshness"),
            ("Last observed", "/last_observed_at"),
            ("Execution control", "/execution_control/state"),
            (
                "Authorization reason",
                "/execution_control/authorization_reason",
            ),
        ],
    );
    if value["cancellation"].is_object() {
        fields(
            &mut output,
            &value["cancellation"],
            &[
                ("Cancellation status", "/status"),
                ("Cancellation requested by", "/requested_by_user_id"),
                ("Cancellation reason", "/reason"),
                ("Cancellation scope", "/scope"),
                ("Cancellation requested", "/requested_at"),
                ("Cancellation confirmed", "/cancelled_at"),
            ],
        );
    }
    let reason = value["blocked_reason"].as_str().or_else(|| {
        value
            .pointer("/execution_control/authorization_reason")
            .and_then(Value::as_str)
    });
    let (role, action) = if matches!(
        value["state"].as_str(),
        Some("succeeded" | "failed" | "cancelled")
    ) {
        (
            "run submitter / model administrator",
            "Inspect the confirmed outcome and retained results.",
        )
    } else {
        match reason {
            Some(
                "model_access_revoked"
                | "publication_revoked"
                | "installation_revoked"
                | "no_approved_execution_path",
            ) => (
                "model administrator",
                "Review the exact accepted publication and its authorization; retain this run ID.",
            ),
            Some("compute_access_revoked" | "compute_unavailable" | "connector_maintenance") => (
                "connector owner",
                "Inspect compute authority, connector maintenance and connector diagnostics.",
            ),
            Some(_) => (
                "unknown",
                "The service reported an unclassified blockage; retain this run ID for diagnosis.",
            ),
            None if value["observation_freshness"] == "stale" => (
                "connector owner",
                "Inspect the original connector and backend evidence; stale observation does not establish failure.",
            ),
            None if value["cancellation"]["status"] == "requested" => (
                "connector owner",
                "Await backend cancellation confirmation; keep observing this run ID.",
            ),
            None if value["state"] == "running" => (
                "connector / backend",
                "Continue observing the reported phase; the recorded evidence establishes no final outcome.",
            ),
            None => (
                "service / connector",
                "Continue observing. Waiting cause is unknown; no confirmed blockage is recorded.",
            ),
        }
    };
    field(
        &mut output,
        "Next responsible role",
        Some(&Value::String(role.into())),
    );
    writeln!(output, "Next action: {action}").expect("string write");
    output
}

pub(crate) fn log_entry(value: Value) -> String {
    let mut output = String::new();
    collections::logs(&mut output, &[value]);
    output
}

pub(crate) fn follow_resume(args: &Args, cursor: Option<&str>) -> String {
    let (mut words, allow_gaps, interval, deadline) = match &args.command {
        Command::Run(crate::args::Run::Logs {
            id,
            state,
            allow_gaps,
            interval,
            deadline,
            ..
        }) => {
            let mut words = vec!["run".into(), "logs".into(), id.clone(), "--follow".into()];
            if let Some(path) = state {
                words.extend(["--state".into(), path.to_string_lossy().into_owned()]);
            } else if let Some(cursor) = cursor {
                words.extend(["--cursor".into(), cursor.into()]);
            }
            (words, allow_gaps, interval, deadline)
        }
        Command::Analysis(crate::args::Analysis::Logs {
            directory,
            allow_gaps,
            interval,
            deadline,
            ..
        }) => (
            vec![
                "analysis".into(),
                "logs".into(),
                directory.to_string_lossy().into_owned(),
                "--follow".into(),
            ],
            allow_gaps,
            interval,
            deadline,
        ),
        _ => return String::new(),
    };
    if *allow_gaps {
        words.push("--allow-gaps".into());
    }
    if let Some(interval) = interval {
        words.extend(["--interval".into(), interval.to_string()]);
    }
    if let Some(deadline) = deadline {
        words.extend(["--deadline".into(), deadline.to_string()]);
    }
    if args.json {
        words.push("--json".into());
    }
    invocation(args, &words)
}

/// Render untrusted terminal text without executing controls or bidi overrides.
/// Newlines are retained only inside explicitly multiline details/log text.
pub(crate) fn safe_text(value: &str, multiline: bool) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if character == '\n' && multiline {
            output.push('\n');
        } else if character.is_control()
            || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            write!(output, "{}", character.escape_unicode()).expect("string write");
        } else {
            output.push(character);
        }
    }
    output
}

fn scalar(value: Option<&Value>) -> String {
    match value {
        None => "unknown / not disclosed".into(),
        Some(Value::Null) => "none recorded".into(),
        Some(Value::String(value)) => safe_text(value, false),
        Some(Value::Bool(value)) => if *value { "yes" } else { "no" }.into(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Array(values)) if values.is_empty() => "none".into(),
        Some(Value::Array(values)) if values.iter().all(|v| !v.is_object() && !v.is_array()) => {
            values
                .iter()
                .map(|v| scalar(Some(v)))
                .collect::<Vec<_>>()
                .join(", ")
        }
        Some(_) => "structured data; use --json".into(),
    }
}

fn field(output: &mut String, label: &str, value: Option<&Value>) {
    if let Some(Value::String(value)) = value {
        let text = safe_text(value, true);
        // Indent continuation lines so content cannot impersonate another field.
        writeln!(output, "{label}: {}", text.replace('\n', "\n  ")).expect("string write");
    } else {
        writeln!(output, "{label}: {}", scalar(value)).expect("string write");
    }
}

fn fields(output: &mut String, value: &Value, columns: &[(&str, &str)]) {
    for (label, path) in columns {
        field(output, label, value.pointer(path));
    }
}

fn present_fields(output: &mut String, value: &Value, columns: &[(&str, &str)]) {
    for (label, path) in columns {
        if let Some(value) = value.pointer(path) {
            field(output, label, Some(value));
        }
    }
}

/// JSON is useful for scientific schemas/results, but never dump an entire response.
fn scientific(output: &mut String, label: &str, value: Option<&Value>) {
    if let Some(value) = value.filter(|value| !value.is_null()) {
        writeln!(output, "{label}:").expect("string write");
        for line in serde_json::to_string_pretty(value)
            .expect("JSON value")
            .lines()
        {
            writeln!(output, "  {}", safe_text(line, false)).expect("string write");
        }
    }
}

fn table(output: &mut String, items: &[Value], columns: &[(&str, &str)]) {
    if items.is_empty() {
        writeln!(output, "No items returned.").expect("string write");
        return;
    }
    let mut shortened = false;
    let rows: Vec<Vec<String>> = std::iter::once(
        columns
            .iter()
            .map(|(label, _)| (*label).to_owned())
            .collect(),
    )
    .chain(items.iter().map(|item| {
        columns
            .iter()
            .map(|(_, path)| {
                let cell = scalar(item.pointer(path));
                if matches!(*path, "/description" | "/message" | "/name" | "/label")
                    && cell.chars().count() > 80
                {
                    shortened = true;
                    format!("{}…", cell.chars().take(80).collect::<String>())
                } else {
                    cell
                }
            })
            .collect()
    }))
    .collect();
    let widths: Vec<usize> = (0..columns.len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            output.push_str(cell);
            if index + 1 < row.len() {
                output.push_str(&" ".repeat(widths[index] - cell.chars().count() + 2));
            }
        }
        output.push('\n');
    }
    if shortened {
        output.push_str(
            "Long descriptions, messages or labels shortened; use --json for complete rows.\n",
        );
    }
}

/// POSIX single-quoting for ordinary words; Bash/Zsh dollar-quotes preserve
/// control characters without sending them to the user's terminal.
fn shell_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-./:".contains(&byte))
    {
        return value.to_owned();
    }
    if safe_text(value, false) != value {
        let mut quoted = String::from("$'");
        for character in value.chars() {
            match character {
                '\'' => quoted.push_str("\\'"),
                '\\' => quoted.push_str("\\\\"),
                character if safe_text(&character.to_string(), false) != character.to_string() => {
                    let mut bytes = [0; 4];
                    for byte in character.encode_utf8(&mut bytes).as_bytes() {
                        write!(quoted, "\\x{byte:02x}").expect("string write");
                    }
                }
                character => quoted.push(character),
            }
        }
        quoted.push('\'');
        quoted
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(crate) fn invocation(args: &Args, words: &[String]) -> String {
    let mut command = vec![
        "enumeray".to_owned(),
        "--profile".into(),
        args.profile.clone(),
    ];
    if let Some(config) = &args.config {
        command.extend(["--config".into(), config.to_string_lossy().into_owned()]);
    }
    if let Some(api) = &args.api {
        command.extend(["--api".into(), api.clone()]);
    }
    command.extend(["--timeout".into(), args.timeout.to_string()]);
    command.extend_from_slice(words);
    command
        .iter()
        .map(|word| shell_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

fn next(output: &mut String, args: &Args, words: &[&str]) {
    let words = words
        .iter()
        .map(|word| (*word).to_owned())
        .collect::<Vec<_>>();
    writeln!(output, "Next: {}", invocation(args, &words)).expect("string write");
}

/// A retention gap is never acknowledged automatically. Keep JSON errors intact.
pub(crate) fn error_hint(args: &Args, error: &enumeray_client::Error) -> Option<String> {
    let enumeray_client::Error::Api {
        status: 410,
        failure,
        ..
    } = error
    else {
        return None;
    };
    if failure.code != "history_cursor_expired" {
        return None;
    }
    let (mut words, page) = match &args.command {
        Command::Run(crate::args::Run::Logs { id, page, .. }) => {
            (vec!["run".into(), "logs".into(), id.clone()], page)
        }
        Command::Run(crate::args::Run::Events { id, page }) => {
            (vec!["run".into(), "events".into(), id.clone()], page)
        }
        Command::Analysis(crate::args::Analysis::Logs {
            directory, page, ..
        }) => (
            vec![
                "analysis".into(),
                "logs".into(),
                directory.to_string_lossy().into_owned(),
            ],
            page,
        ),
        _ => return None,
    };
    if let Some(limit) = page.limit {
        words.extend(["--limit".into(), limit.to_string()]);
    }
    Some(format!(
        "History has a retention gap. To explicitly acknowledge missing earlier entries and read the earliest retained history, restart without --cursor:\n{}",
        invocation(args, &words)
    ))
}

#[cfg(test)]
mod tests;
