//! Read-only observation. Process outcomes and streamed output never alter run identity.
mod checkpoint;

use crate::{
    app::{Completion, Failure},
    args::{Analysis, Args, Command, PageArgs, Run},
    output,
};
use checkpoint::Checkpoint;
use enumeray_client::{
    CancellationToken, EnumerayClient, Error,
    observation::{
        LogEntry, LogHistory, LogStreamData, LogStreamEvent, ObservationClient, ObservationError,
        RetentionBoundary,
    },
};
use serde_json::{Value, json};
use std::{
    io::{Write, stdout},
    path::Path,
    time::Duration,
};
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep, sleep_until},
};

impl From<ObservationError> for Failure {
    fn from(error: ObservationError) -> Self {
        match error {
            ObservationError::Request(error) => error.into(),
            ObservationError::RetentionGap(_) => Error::Invalid(
                "Log history has a retention gap; explicitly acknowledge it with --allow-gaps."
                    .into(),
            )
            .into(),
        }
    }
}

pub(crate) fn execute(args: &Args, client: &EnumerayClient) -> Result<Completion, Failure> {
    let Command::Run(command) = &args.command else {
        unreachable!()
    };
    execute_command(args, client, command)
}

/// Begin the interrupt/deadline scope before resolving this directory's account and receipt.
pub(crate) fn analysis(args: &Args, client: &EnumerayClient) -> Result<Completion, Failure> {
    match &args.command {
        Command::Analysis(Analysis::Wait {
            interval,
            deadline,
            check_outcome,
            ..
        }) => wait_run(
            args,
            client,
            String::new(),
            *interval,
            *deadline,
            *check_outcome,
        ),
        Command::Analysis(Analysis::Logs {
            directory,
            interval,
            deadline,
            allow_gaps,
            follow: true,
            ..
        }) => follow_run(
            args,
            client,
            String::new(),
            *interval,
            *deadline,
            &directory.join("state/logs.json"),
            *allow_gaps,
        ),
        _ => unreachable!("analysis observation dispatch"),
    }
}

pub(crate) fn wait_run(
    args: &Args,
    client: &EnumerayClient,
    id: String,
    interval: u64,
    deadline: u64,
    check_outcome: bool,
) -> Result<Completion, Failure> {
    execute_command(
        args,
        client,
        &Run::Wait {
            id,
            interval,
            deadline,
            check_outcome,
        },
    )
}

pub(crate) fn follow_run(
    args: &Args,
    client: &EnumerayClient,
    id: String,
    interval: Option<u64>,
    deadline: Option<u64>,
    state: &Path,
    allow_gaps: bool,
) -> Result<Completion, Failure> {
    execute_command(
        args,
        client,
        &Run::Logs {
            id,
            page: PageArgs::default(),
            follow: true,
            interval,
            deadline,
            state: Some(state.to_path_buf()),
            allow_gaps,
        },
    )
}

fn observation_stopped(args: &Args, reason: &str) -> Error {
    let action = match &args.command {
        Command::Analysis(Analysis::Wait { directory, .. } | Analysis::Logs { directory, .. }) => {
            format!("Inspect analysis: {}", analysis_status(args, directory))
        }
        _ => "Inspect the same run ID.".into(),
    };
    Error::Invalid(format!(
        "Observation {reason}; execution is unchanged. {action}"
    ))
}

fn analysis_status(args: &Args, directory: &Path) -> String {
    output::invocation(
        args,
        &[
            "analysis".into(),
            "status".into(),
            directory.to_string_lossy().into_owned(),
        ],
    )
}

fn execute_command(
    args: &Args,
    client: &EnumerayClient,
    command: &Run,
) -> Result<Completion, Failure> {
    let (seconds, deadline_exit) = match command {
        Run::Wait {
            deadline,
            check_outcome,
            ..
        } => (*deadline, if *check_outcome { 5 } else { 1 }),
        Run::Logs { deadline, .. } => (deadline.unwrap_or(3600), 5),
        _ => unreachable!(),
    };
    if args.timeout == 0
        || std::time::Instant::now()
            .checked_add(Duration::from_secs(args.timeout))
            .is_none()
    {
        return Err(
            Error::Invalid("HTTP timeout must be positive and representable.".into()).into(),
        );
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::from)?;
    let cancellation = CancellationToken::new();
    let client = client.with_cancellation(cancellation.clone());
    let result = runtime.block_on(async {
        let until = Instant::now().checked_add(Duration::from_secs(seconds))
            .ok_or_else(|| Error::Invalid("Observation deadline is too large.".into()))?;
        let client = client.with_deadline(until.into_std());
        let reader = client.observation_client()?;
        let mut resume_cursor = match command { Run::Logs { page, .. } => page.cursor.clone(), _ => None };
        let result = {
            let observe = async {
                let resolved = match &args.command {
                    Command::Analysis(Analysis::Wait { directory, .. } | Analysis::Logs { directory, follow: true, .. }) => {
                        let user = reader.me().await?.user_id;
                        let target = crate::analysis::observation_target(client.base_url(), user, directory)?;
                        let bound = match command {
                            Run::Wait { interval, deadline, check_outcome, .. } => Run::Wait {
                                id: target.run_id.clone(), interval: *interval, deadline: *deadline, check_outcome: *check_outcome,
                            },
                            Run::Logs { interval, deadline, allow_gaps, .. } => Run::Logs {
                                id: target.run_id.clone(), page: PageArgs::default(), follow: true,
                                interval: *interval, deadline: *deadline,
                                state: Some(target.root.join("state/logs.json")), allow_gaps: *allow_gaps,
                            },
                            _ => unreachable!("analysis observation command"),
                        };
                        Some((target, bound, user))
                    }
                    _ => None,
                };
                let command = resolved.as_ref().map_or(command, |(_, bound, _)| bound);
                let target = resolved.as_ref().map(|(target, _, _)| target);
                let result = match command {
                    Run::Wait { id, interval, check_outcome, .. } => wait(&reader, id, *interval, *check_outcome, until, target).await,
                    Run::Logs { id, interval, state, allow_gaps, .. } => {
                        let user = if let Some((_, _, user)) = &resolved { *user }
                            else if state.is_some() { reader.me().await?.user_id }
                            else { uuid::Uuid::nil() };
                        let checkpoint = Checkpoint::open(state.as_deref(), client.base_url(), user, id, resume_cursor.as_deref())
                            .map_err(|error| match error {
                                Error::Invalid(message) if matches!(args.command, Command::Analysis(Analysis::Logs { .. }))
                                    && message.starts_with("Log checkpoint is already in use") => Error::Invalid(
                                        "Analysis log following is already in use or its private checkpoint cannot be locked; use one follower per directory.".into()
                                    ),
                                error => error,
                            })?;
                        resume_cursor = checkpoint.cursor().map(str::to_owned);
                        let mut sink = FollowOutput { writer: stdout(), json: args.json, checkpoint, allow_gaps: *allow_gaps, position: 0 };
                        sink.position = sink.checkpoint.position();
                        let result = follow(&reader, id, interval.unwrap_or(2), &mut sink, until, &mut resume_cursor, target).await;
                        resume_cursor = sink.checkpoint.cursor().map(str::to_owned);
                        result
                    }
                    _ => unreachable!(),
                };
                if let (Some(target), Ok(Completion { value: Some(value), .. })) = (target, &result) {
                    target.finish(value, true)?;
                }
                result
            };
            tokio::select! {
                biased;
                signal = tokio::signal::ctrl_c() => {
                    cancellation.cancel();
                    signal.map_err(Error::from)?;
                    Err(Failure { error: observation_stopped(args, "interrupted"), exit: 130 })
                }
                () = sleep_until(until) => {
                    cancellation.cancel();
                    Err(Failure { error: observation_stopped(args, "deadline elapsed"), exit: deadline_exit })
                },
                result = observe => result,
            }
        };
        if let Err(failure) = &result {
            if matches!(&failure.error, Error::Api { status: 401 | 403, .. }) {
                eprintln!("Read authorization is unavailable. Check scopes and current authority; renew the same account with auth refresh/login or replace its scoped token, then resume observation without submitting another run.");
            }
            if matches!(command, Run::Logs { .. }) {
                eprintln!("Resume observation: {}", output::follow_resume(args, resume_cursor.as_deref()));
            } else if let Command::Analysis(Analysis::Wait { directory, .. }) = &args.command {
                eprintln!("Inspect analysis: {}", analysis_status(args, directory));
            }
        }
        result
    });
    cancellation.cancel();
    // Acquisition workers may finish saving an already-issued refresh. Do not
    // block cancellation while those bounded workers finish credential cleanup.
    runtime.shutdown_background();
    result
}

fn state(value: &Value) -> Result<&str, Error> {
    match value["state"].as_str() {
        Some(state @ ("queued" | "running" | "succeeded" | "failed" | "cancelled")) => Ok(state),
        _ => Err(Error::Invalid(
            "Run response omitted a recognized state.".into(),
        )),
    }
}

fn terminal(value: &Value) -> Result<bool, Error> {
    Ok(matches!(
        state(value)?,
        "succeeded" | "failed" | "cancelled"
    ))
}

#[derive(Default)]
struct Progress(Option<Value>);

impl Progress {
    fn observe(&mut self, id: &str, value: &Value) {
        let fingerprint = json!([
            value["state"],
            value["phase"],
            value["blocked_reason"],
            value["observation_freshness"],
            value["execution_control"],
            value["cancellation"]
        ]);
        if self.0.as_ref() != Some(&fingerprint) {
            eprintln!(
                "Run {}:\n{}",
                output::safe_text(id, false),
                output::observation(value).trim_end()
            );
            self.0 = Some(fingerprint);
        }
    }
}

async fn wait(
    client: &ObservationClient,
    id: &str,
    interval: u64,
    check: bool,
    until: Instant,
    target: Option<&crate::analysis::ObservationTarget>,
) -> Result<Completion, Failure> {
    let mut progress = Progress::default();
    loop {
        let value = client.run(id).await?;
        if let Some(target) = target {
            target.validate(&value)?;
        }
        let terminal = terminal(&value)?;
        progress.observe(id, &value);
        if terminal {
            let exit = if check {
                match state(&value)? {
                    "failed" => 3,
                    "cancelled" => 4,
                    _ => 0,
                }
            } else {
                0
            };
            return Ok(Completion {
                value: Some(value),
                exit,
            });
        }
        sleep(Duration::from_secs(interval).min(until.saturating_duration_since(Instant::now())))
            .await;
    }
}

struct FollowOutput<W: Write> {
    writer: W,
    json: bool,
    checkpoint: Checkpoint,
    allow_gaps: bool,
    position: i64,
}

impl<W: Write> FollowOutput<W> {
    fn event(&mut self, name: &str, id: Option<&str>, data: Value) -> Result<(), Error> {
        if self.json {
            writeln!(self.writer, "{}", json!({"event":name,"id":id,"data":data}))?;
        } else if name == "log" {
            write!(self.writer, "{}", output::log_entry(data))?;
        } else if name == "run" {
            eprintln!(
                "Final run observation:\n{}",
                output::observation(&data).trim_end()
            );
        } else {
            eprintln!(
                "Log stream {}: {}",
                output::safe_text(name, false),
                output::safe_text(&data.to_string(), false)
            );
        }
        self.writer.flush()?;
        Ok(())
    }

    fn log(&mut self, entry: LogEntry, cursor: Option<&str>) -> Result<(), Error> {
        if entry.position <= self.position {
            return Ok(());
        }
        let position = entry.position;
        self.event("log", cursor, serde_json::to_value(entry)?)?;
        // Paged history supplies a cursor only after the whole page has been processed.
        if let Some(cursor) = cursor {
            self.checkpoint.advance(Some(cursor), position)?;
        }
        self.position = position;
        Ok(())
    }

    fn gap(&mut self, boundary: RetentionBoundary) -> Result<(), Error> {
        self.event(
            "retention_gap",
            Some(&boundary.recovery_cursor),
            serde_json::to_value(&boundary)?,
        )?;
        if !self.allow_gaps {
            return Err(Error::Invalid("Log retention gap: earlier entries are missing. Review the gap, then explicitly resume with --allow-gaps to acknowledge the omitted history.".into()));
        }
        let position = boundary
            .earliest_position
            .map_or(boundary.latest_position, |n| n.saturating_sub(1))
            .max(self.position);
        self.checkpoint
            .advance(Some(&boundary.recovery_cursor), position)?;
        self.position = position;
        Ok(())
    }

    fn page(&mut self, page: LogHistory) -> Result<bool, Error> {
        if self.checkpoint.cursor().is_none()
            && page
                .retention
                .earliest_position
                .map_or(page.retention.latest_position, |n| n - 1)
                > 0
        {
            self.gap(page.retention)?;
        }
        let count = page.items.len();
        for (index, entry) in page.items.into_iter().enumerate() {
            // Only the page boundary has a server-issued cursor. Never synthesize IDs.
            self.log(
                entry,
                (index + 1 == count).then_some(page.resume_cursor.as_str()),
            )?;
        }
        self.checkpoint
            .advance(Some(&page.resume_cursor), self.position)?;
        Ok(page.next_cursor.is_some())
    }
}

enum Message {
    Event(LogStreamEvent),
    RenewalDue,
    End,
    Error(ObservationError),
}

/// Dropping an interrupted observer also cancels its bounded producer.
struct StreamTask(JoinHandle<()>);
impl Drop for StreamTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn open_stream(
    client: ObservationClient,
    id: String,
    cursor: Option<String>,
) -> (StreamTask, mpsc::Receiver<Message>) {
    let (sender, receiver) = mpsc::channel(1);
    let task = tokio::spawn(async move {
        let result = async {
            let mut stream = client.log_stream(&id, cursor.as_deref()).await?;
            // This is the lease used by this particular connection. Polling can
            // renew the shared source while an older stream remains open.
            let renew_at = stream.renewal_deadline().map(Instant::from_std);
            loop {
                let next = tokio::select! {
                    biased;
                    () = async {
                        if let Some(deadline) = renew_at {
                            sleep_until(deadline).await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    } => {
                        let _ = sender.send(Message::RenewalDue).await;
                        return Ok(());
                    }
                    next = stream.next() => next.map_err(ObservationError::from)?,
                };
                match next {
                    Some(event) => {
                        if sender.send(Message::Event(event)).await.is_err() {
                            return Ok(());
                        }
                    }
                    None => {
                        let _ = sender.send(Message::End).await;
                        return Ok(());
                    }
                }
            }
        }
        .await;
        if let Err(error) = result {
            let _ = sender.send(Message::Error(error)).await;
        }
    });
    (StreamTask(task), receiver)
}

async fn inspect_after(
    client: &ObservationClient,
    id: &str,
    delay: Duration,
    until: Instant,
) -> Result<Value, ObservationError> {
    sleep(delay.min(until.saturating_duration_since(Instant::now()))).await;
    client.run(id).await
}

async fn follow<W: Write>(
    client: &ObservationClient,
    id: &str,
    interval: u64,
    sink: &mut FollowOutput<W>,
    until: Instant,
    resume_cursor: &mut Option<String>,
    target: Option<&crate::analysis::ObservationTarget>,
) -> Result<Completion, Failure> {
    let mut progress = Progress::default();
    let (task, mut receiver) = open_stream(
        client.clone(),
        id.into(),
        sink.checkpoint.cursor().map(str::to_owned),
    );
    let mut task = Some(task);
    let mut inspection = Box::pin(inspect_after(client, id, Duration::ZERO, until));
    let mut reconnect_at = Instant::now();
    let mut backoff = 1;
    let mut inspect_backoff = 1;
    loop {
        tokio::select! {
            result = &mut inspection => {
                match result {
                    Ok(value) => {
                        if let Some(target) = target { target.validate(&value)?; }
                        let terminal = terminal(&value)?;
                        progress.observe(id, &value);
                        if terminal {
                            drop(task.take());
                            loop {
                                match client.logs(id, sink.checkpoint.cursor()).await {
                                    Ok(page) => { let more = sink.page(page)?; *resume_cursor = sink.checkpoint.cursor().map(str::to_owned); if !more { break; } },
                                    Err(ObservationError::RetentionGap(boundary)) => { sink.gap(boundary)?; *resume_cursor = sink.checkpoint.cursor().map(str::to_owned); },
                                    Err(error) => {
                                        let Some(delay) = transient_delay(&error, inspect_backoff, until) else { return Err(error.into()); };
                                        eprintln!("Final retained-log read interrupted; retrying the same run.");
                                        inspect_backoff = (inspect_backoff * 2).min(30);
                                        sleep(delay).await;
                                    }
                                }
                            }
                            if let Some(target) = target { target.finish(&value, false)?; }
                            sink.event("run", None, value)?;
                            return Ok(Completion { value: None, exit: 0 });
                        }
                        inspect_backoff = 1;
                        inspection = Box::pin(inspect_after(client, id, Duration::from_secs(interval), until));
                    }
                    Err(error) => {
                        let Some(delay) = transient_delay(&error, inspect_backoff, until) else { return Err(error.into()); };
                        inspect_backoff = (inspect_backoff * 2).min(30);
                        eprintln!("Run inspection interrupted; retrying the same run.");
                        inspection = Box::pin(inspect_after(client, id, delay, until));
                    }
                }
            }
            message = receiver.recv(), if task.is_some() => {
                let delay = match message {
                    Some(Message::RenewalDue) => {
                        drop(task.take());
                        reconnect_at = Instant::now();
                        continue;
                    }
                    Some(Message::Event(event)) => match event.data {
                        LogStreamData::Log(entry) => { sink.log(entry, event.id.as_deref())?; *resume_cursor = sink.checkpoint.cursor().map(str::to_owned); backoff = 1; continue; }
                        LogStreamData::RetentionGap(boundary) => { sink.gap(boundary)?; *resume_cursor = sink.checkpoint.cursor().map(str::to_owned); continue; }
                        LogStreamData::AuthRevoked => {
                            sink.event("auth_revoked", None, json!({"code":"auth_revoked"}))?;
                            return Err(Error::Invalid("Live log authorization expired or was revoked. Renew the same account with auth refresh/login or replace its scoped token, then resume; no new submission is needed.".into()).into());
                        }
                        LogStreamData::SourceUnavailable | LogStreamData::Shutdown => {
                            let name = match event.data { LogStreamData::Shutdown => "shutdown", _ => "source_unavailable" };
                            sink.event(name, None, json!({"code":name}))?;
                            Duration::from_secs(backoff)
                        }
                    },
                    Some(Message::Error(ObservationError::RetentionGap(boundary))) => { sink.gap(boundary)?; *resume_cursor = sink.checkpoint.cursor().map(str::to_owned); Duration::ZERO }
                    Some(Message::Error(error)) => {
                        let Some(delay) = transient_delay(&error, backoff, until) else { return Err(error.into()); };
                        delay
                    }
                    _ => Duration::from_secs(backoff),
                };
                drop(task.take());
                eprintln!("Log connection interrupted; reconnecting from the last processed cursor.");
                reconnect_at = Instant::now() + delay.min(until.saturating_duration_since(Instant::now()));
                backoff = (backoff * 2).min(30);
            }
            () = sleep_until(reconnect_at), if task.is_none() => {
                let (next, channel) = open_stream(client.clone(), id.into(), sink.checkpoint.cursor().map(str::to_owned));
                task = Some(next); receiver = channel;
            }
        }
    }
}

fn transient_delay(error: &ObservationError, backoff: u64, until: Instant) -> Option<Duration> {
    let ObservationError::Request(error) = error else {
        return None;
    };
    let retry = match error {
        Error::Transport(error)
            if error.is_connect()
                || error.is_timeout()
                || error.is_body()
                || error.is_request() =>
        {
            None
        }
        Error::Api {
            status: 429 | 500..=599,
            retry_after,
            ..
        } => retry_after.as_deref(),
        _ => return None,
    };
    let delay = retry
        .and_then(retry_after)
        .unwrap_or(Duration::from_secs(backoff));
    Some(delay.min(until.saturating_duration_since(Instant::now())))
}

fn retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let format = time::format_description::parse_borrowed::<2>(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT",
    )
    .ok()?;
    let deadline = time::PrimitiveDateTime::parse(value, &format)
        .ok()?
        .assume_utc();
    Some(Duration::from_secs(
        (deadline.unix_timestamp() - time::OffsetDateTime::now_utc().unix_timestamp()).max(0)
            as u64,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn progress_ignores_observation_timestamp_refreshes_and_tracks_each_evidence_change() {
        let mut progress = Progress::default();
        let mut value = json!({"state":"running","phase":"preparing","observation_freshness":"fresh","last_observed_at":"first"});
        progress.observe("run", &value);
        let original = progress.0.clone();
        value["last_observed_at"] = json!("later");
        progress.observe("run", &value);
        assert_eq!(progress.0, original);
        for (key, changed) in [
            ("phase", json!("executing")),
            ("blocked_reason", json!("compute_access_revoked")),
            ("observation_freshness", json!("stale")),
            ("observation_freshness", json!("unknown")),
            ("execution_control", json!({"state":"draining"})),
            ("cancellation", json!({"status":"requested"})),
            ("cancellation", json!({"status":"confirmed"})),
            ("state", json!("cancelled")),
        ] {
            let previous = progress.0.clone();
            value[key] = changed;
            progress.observe("run", &value);
            assert_ne!(progress.0, previous, "{key}");
        }
    }

    #[test]
    fn failed_output_flush_never_advances_checkpoint() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let checkpoint = Checkpoint::open(
            Some(&path),
            "https://example.org/",
            uuid::Uuid::nil(),
            "run",
            None,
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut sink = FollowOutput {
            writer: Broken,
            json: true,
            checkpoint,
            allow_gaps: false,
            position: 0,
        };
        let entry: LogEntry = serde_json::from_value(json!({"position":1,"assignment_id":uuid::Uuid::nil(),"source":"stdout",
            "epoch":1,"sequence":1,"offset":0,"byte_length":1,"text":"x","gap":null,"source_reset":false,"received_at":"now"})).unwrap();
        assert!(sink.log(entry, Some("one")).is_err());
        assert_eq!(sink.checkpoint.position(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn retry_after_supports_seconds_dates_and_remaining_deadline() {
        assert_eq!(retry_after("12"), Some(Duration::from_secs(12)));
        assert_eq!(
            retry_after("Thu, 15 Oct 2020 12:00:00 GMT"),
            Some(Duration::ZERO)
        );
        assert!(retry_after("Thu, 15 Oct 2037 12:00:00 GMT").unwrap() > Duration::from_secs(1));
        assert!(retry_after("invalid").is_none());
        let until = Instant::now() + Duration::from_secs(2);
        let error = ObservationError::Request(Error::Api {
            status: 429,
            failure: Box::new(enumeray_client::ApiFailure {
                code: "limit".into(),
                message: "retry".into(),
                request_id: "request".into(),
                details: None,
            }),
            retry_after: Some("300".into()),
        });
        assert!(transient_delay(&error, 1, until).unwrap() <= Duration::from_secs(2));
    }
}
