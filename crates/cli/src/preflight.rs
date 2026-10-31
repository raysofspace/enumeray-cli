//! Saved owner diagnostics. Repeating a directory reconciles the same request.
use crate::{
    app::{Completion, Failure},
    config,
};
use enumeray_client::{CancellationToken, EnumerayClient, Error};
use enumeray_contracts::setup::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    manifest_version: u32,
    request_id: Uuid,
    api_url: String,
    user_id: Uuid,
    connector_id: String,
    context_id: String,
    request: PreflightRequest,
}

pub(crate) fn execute(
    client: &EnumerayClient,
    connector: &str,
    context: &str,
    directory: &Path,
    interval: u64,
    seconds: u64,
) -> Result<Completion, Failure> {
    let until = Instant::now()
        .checked_add(Duration::from_secs(seconds))
        .ok_or_else(|| Error::Invalid("Preflight deadline is too large.".into()))?;
    let cancellation = CancellationToken::new();
    let client = client
        .with_cancellation(cancellation.clone())
        .with_deadline(until);
    let connector = connector.to_owned();
    let context = context.to_owned();
    let directory = directory.to_path_buf();
    let resume_directory = directory.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::from)?;
    let result=runtime.block_on(async {
        tokio::select! {
            biased;
            signal=tokio::signal::ctrl_c()=> {
                cancellation.cancel(); stop.store(true,Ordering::Release); signal.map_err(Error::from)?;
                Ok(Completion {value:Some(json!({"directory":resume_directory,"status":"interrupted","next_action":"Resume the same preflight directory to reconcile its request."})),exit:130})
            },
            ()=tokio::time::sleep_until(until.into())=> {
                cancellation.cancel(); stop.store(true,Ordering::Release);
                Ok(Completion {value:Some(json!({"directory":resume_directory,"status":"deadline","next_action":"Resume the same preflight directory; the server attempt may still complete."})),exit:5})
            },
            result=tokio::task::spawn_blocking(move||run(&client,&connector,&context,&directory,interval,&stopping))=>result.map_err(|_|Error::Invalid("Preflight observer stopped; retain its operation directory.".into()))?.map_err(Into::into),
        }
    });
    runtime.shutdown_background();
    result
}
fn run(
    client: &EnumerayClient,
    connector: &str,
    context: &str,
    directory: &Path,
    interval: u64,
    stop: &AtomicBool,
) -> Result<Completion, Error> {
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(Error::Invalid(
            "Preflight operation directory must be a private real directory.".into(),
        ));
    }
    let path = directory.join("operation.json");
    let _lock = config::lock_private(&path, "This preflight operation is already in use.")?;
    let user = client.me()?.user_id;
    let operation = if path.exists() {
        config::require_private(&path)?;
        let saved: Operation = config::read_json(&path)?;
        if saved.manifest_version != 1
            || saved.api_url != client.base_url()
            || saved.user_id != user
            || saved.connector_id != connector
            || saved.context_id != context
            || saved.request_id.is_nil()
        {
            return Err(Error::Invalid("Preflight operation belongs to another API, account or context; keep the saved identity.".into()));
        }
        saved
    } else {
        let current = client.management().context_preflight(connector, context)?;
        let saved = Operation {
            manifest_version: 1,
            request_id: Uuid::new_v4(),
            api_url: client.base_url().into(),
            user_id: user,
            connector_id: connector.into(),
            context_id: context.into(),
            request: PreflightRequest {
                expected_previous_request_id: current.attempt.map(|a| a.request_id),
                expected_ownership_generation: current.ownership_generation,
                definition_hash: current.definition_hash,
            },
        };
        config::write_private(&path, &saved, false)?;
        saved
    };
    if stop.load(Ordering::Acquire) {
        return Err(Error::Invalid(
            "Preflight interrupted; resume the same directory.".into(),
        ));
    }
    let mut status = client.management().context_preflight(connector, context)?;
    if status
        .attempt
        .as_ref()
        .is_none_or(|a| a.request_id != operation.request_id)
    {
        status = client.management().request_preflight(
            connector,
            context,
            operation.request_id,
            &operation.request,
        )?;
    }
    loop {
        if status.connector_id != connector
            || status.context_id != context
            || status
                .attempt
                .as_ref()
                .is_none_or(|a| a.request_id != operation.request_id)
        {
            return Err(Error::Invalid("Preflight attempt was superseded or its response identity changed; preserve this directory and inspect current status.".into()));
        }
        let terminal = status
            .attempt
            .as_ref()
            .is_some_and(|a| a.state != AttemptState::Pending);
        if status
            .attempt
            .as_ref()
            .is_some_and(|a| a.state == AttemptState::Completed)
            && status.report.as_ref().is_none_or(|r| {
                r.source != ReportSource::Connector
                    || r.request_id != operation.request_id
                    || r.connector_id != connector
                    || r.context_id != context
                    || r.definition_hash != status.definition_hash
            })
        {
            return Err(Error::Invalid(
                "Completed preflight report does not match the saved request and context.".into(),
            ));
        }
        if status.ready
            && (status.validity != ReportValidity::Current
                || status.report.as_ref().is_none_or(|r| !r.ready()))
        {
            return Err(Error::Invalid(
                "Preflight readiness lacks current passed host evidence.".into(),
            ));
        }
        if terminal {
            config::write_private(&directory.join("report.json"), &status, true)?;
            let exit = u8::from(!status.ready);
            let mut value: Value = serde_json::to_value(status)?;
            value["directory"] = json!(directory);
            return Ok(Completion {
                value: Some(value),
                exit,
            });
        }
        for _ in 0..interval * 10 {
            if stop.load(Ordering::Acquire) {
                return Err(Error::Invalid(
                    "Preflight observation stopped; resume the same directory.".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        status = client.management().context_preflight(connector, context)?;
    }
}
