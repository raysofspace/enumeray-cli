//! Low-priority diagnostics, independent of publication configuration and model workers.
use crate::{
    Result,
    client::Client,
    failure, local_process,
    preflight::{self, HostEnvironment, WorkerRequest},
};
use enumeray_contracts::setup::*;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedReport {
    session_id: Uuid,
    report: HostPreflightReport,
}

pub(crate) struct Worker {
    send: mpsc::SyncSender<Uuid>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Worker {
    pub(crate) fn start(
        client: Client,
        directory: PathBuf,
        mut environment: HostEnvironment,
    ) -> Self {
        let (send, receive) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            // Host CLI paths may be relative; runner-side checks need this host's
            // absolute paths, without changing the journal's retained identity.
            for path in [&mut environment.journal, &mut environment.token_file]
                .into_iter()
                .chain(environment.credentials_file.as_mut())
            {
                if let Ok(absolute) = std::path::absolute(&*path) {
                    *path = absolute.to_string_lossy().into_owned();
                }
            }
            environment.account = match preflight::current_account() {
                Ok(account) => account,
                Err(_) => return,
            };
            let mut previous_error = false;
            while !stopping.load(Ordering::Acquire) {
                let session = match receive.recv_timeout(Duration::from_millis(250)) {
                    Ok(session) => session,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                };
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                let result = once(&client, &directory, &environment, session, &stopping);
                if result.is_err() && !previous_error {
                    eprintln!(
                        "{}",
                        json!({"status":"host_preflight_unavailable","message":"Host diagnostics could not be reconciled; existing model execution and recovery remain independent."})
                    );
                }
                previous_error = result.is_err();
            }
        });
        Self {
            send,
            stop,
            thread: Some(thread),
        }
    }
    pub(crate) fn schedule(&self, session: Uuid) {
        let _ = self.send.try_send(session);
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.send.try_send(Uuid::nil());
        if let Some(thread) = self.thread.take() {
            // Give the supervised child a bounded opportunity to terminate its
            // diagnostic process group before this process exits. A stalled API
            // request must still never hold shutdown until its network timeout.
            let until = std::time::Instant::now() + Duration::from_millis(500);
            while !thread.is_finished() && std::time::Instant::now() < until {
                thread::sleep(Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}
fn once(
    client: &Client,
    directory: &std::path::Path,
    environment: &HostEnvironment,
    session: Uuid,
    stop: &AtomicBool,
) -> Result<()> {
    let (status, bytes) = client.request(
        Method::GET,
        &format!("/connector/v1/preflights?session_id={session}"),
        None,
        None,
        None,
    )?;
    if status != StatusCode::OK {
        return Err(failure("host preflight polling unavailable"));
    }
    let pending: Vec<PendingPreflight> = serde_json::from_slice(&bytes)
        .map_err(|_| failure("invalid pending preflight response"))?;
    if pending.len() > 1 {
        return Err(failure("pending preflight response exceeds bound"));
    }
    let Some(work) = pending.into_iter().next() else {
        return Ok(());
    };
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let cached =
        local_process::read_record::<SavedReport>(&directory.join("preflight-report.json")).ok();
    let report = if let Some(saved) = cached.filter(|saved| {
        saved.session_id == session
            && saved.report.request_id == work.request_id
            && saved.report.connector_id == work.connector_id
            && saved.report.context_id == work.context_id
            && saved.report.source == ReportSource::Connector
            && saved.report.validate(&work.definition).is_ok()
    }) {
        saved.report
    } else {
        let request = WorkerRequest {
            request_id: work.request_id,
            // Retain the diagnostic file identity across a replacement session.
            // Existing files then produce an explicit incomplete-cleanup report.
            probe_id: work.request_id,
            connector_id: work.connector_id,
            context_id: work.context_id,
            definition: work.definition,
            environment: environment.clone(),
            source: ReportSource::Connector,
        };
        preflight::supervise_cancellable(&request, stop)?
    };
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let saved = serde_json::to_vec(&SavedReport {
        session_id: session,
        report: report.clone(),
    })
    .map_err(|_| failure("cannot encode private preflight evidence"))?;
    // This is the bounded latest diagnostic snapshot, not immutable launch evidence.
    crate::control::atomic_write(directory, "preflight-report.json", &saved)?;
    let submission = serde_json::to_value(PreflightSubmission {
        session_id: session,
        report,
    })
    .map_err(|_| failure("cannot encode preflight submission"))?;
    let (status, _) = client.request(
        Method::PUT,
        &format!("/connector/v1/preflights/{}/report", work.request_id),
        None,
        None,
        Some(&submission),
    )?;
    if status != StatusCode::OK {
        return Err(failure(
            "host preflight report is not acknowledged; retained for reconciliation",
        ));
    }
    Ok(())
}
