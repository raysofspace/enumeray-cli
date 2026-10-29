//! Exact durable deliveries over bounded source ranges; rotation never regenerates a pending request.
use super::{
    Result,
    client::{Client, http_error, parse_json},
    failure,
    journal::{self, Journal, PreparedAssignment},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use enumeray_contracts::logs::{
    CHUNK_BYTES, LogBatchRequest, LogGap, LogGapReason, LogSource, READ_WINDOW_BYTES,
    ReadLogsRequest, ReadLogsResponse,
};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, mpsc},
    thread,
};
use uuid::Uuid;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    streams: BTreeMap<LogSource, Stream>,
    cursor: usize,
}
impl Progress {
    pub(crate) fn summary(&self) -> serde_json::Value {
        serde_json::Value::Object(self.streams.iter().map(|(source, stream)|
            (source.as_str().to_owned(), json!({"epoch":stream.epoch,"byte_offset":stream.byte_offset,
                "next_sequence":stream.next_sequence,"closed":stream.closed,"pending":stream.pending.is_some()})))
            .collect())
    }

    pub(crate) fn delivered(
        &self,
        assignment: &enumeray_contracts::assignments::claims::AssignmentReceipt,
    ) -> bool {
        enumeray_contracts::logs::expected_sources(
            &assignment.definition,
            assignment.requirements.driver == enumeray_contracts::execution::ExecutionDriver::Slurm,
        )
        .into_iter()
        .all(|source| {
            self.streams
                .get(&source)
                .is_some_and(|s| s.closed && s.pending.is_none())
        })
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Stream {
    epoch: i64,
    byte_offset: u64,
    next_sequence: i64,
    closed: bool,
    pending: Option<Pending>,
}
impl Default for Stream {
    fn default() -> Self {
        Self {
            epoch: 1,
            byte_offset: 0,
            next_sequence: 1,
            closed: false,
            pending: None,
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    epoch: i64,
    sequence: i64,
    byte_offset_after: u64,
    closed_after: bool,
    request: LogBatchRequest,
}
struct ReadTask {
    request: ReadLogsRequest,
    receiver: mpsc::Receiver<Result<ReadLogsResponse>>,
    thread: thread::JoinHandle<()>,
}
struct Window {
    response: ReadLogsResponse,
    bytes: Vec<u8>,
    complete: bool,
}
impl Window {
    fn new(mut response: ReadLogsResponse, complete: bool) -> Result<Self> {
        let bytes = STANDARD
            .decode(&response.data)
            .map_err(|_| failure("invalid runner log bytes"))?;
        if bytes.len() > READ_WINDOW_BYTES
            || response.epoch < 1
            || response.earliest > response.offset
            || response.offset > response.end
            || response.end > i64::MAX as u64
            || response
                .offset
                .checked_add(bytes.len() as u64)
                .is_none_or(|end| end > response.end)
        {
            return Err(failure("runner read window exceeds capture bounds"));
        }
        response.data = String::new();
        Ok(Self {
            response,
            bytes,
            complete,
        })
    }
    fn chunk(&self, offset: u64) -> ReadLogsResponse {
        let start = (offset - self.response.offset) as usize;
        let mut response = self.response.clone();
        response.offset = offset;
        response.data =
            STANDARD.encode(&self.bytes[start..self.bytes.len().min(start + CHUNK_BYTES)]);
        response
    }
    fn cached(&self, request: &ReadLogsRequest) -> Option<ReadLogsResponse> {
        if request.epoch != self.response.epoch || request.complete != self.complete {
            return None;
        }
        let offset = request.offset.max(self.response.earliest);
        let start = usize::try_from(offset.checked_sub(self.response.offset)?).ok()?;
        if start > self.bytes.len()
            || start == self.bytes.len() && (!self.response.closed || offset != self.response.end)
            || self.bytes.len() - start < CHUNK_BYTES && !self.response.closed
        {
            return None;
        }
        Some(self.chunk(offset))
    }
}
#[derive(Default)]
struct ReadState {
    tasks: BTreeMap<Uuid, ReadTask>,
    windows: BTreeMap<(Uuid, LogSource), Window>,
}
#[derive(Clone, Default)]
pub(crate) struct Readers(Arc<Mutex<ReadState>>);

fn read_source(
    client: &Client,
    journal: &Journal,
    intent: Uuid,
    request: ReadLogsRequest,
) -> Result<ReadLogsResponse> {
    let mut readers = client.log_readers.0.lock().expect("log readers");
    readers.windows.retain(|(id, _), _| {
        journal
            .data
            .prepared_assignments
            .iter()
            .any(|entry| entry.launch_intent_id == Some(*id))
    });
    let key = (intent, request.source);
    if let Some(response) = readers
        .windows
        .get(&key)
        .and_then(|window| window.cached(&request))
    {
        return Ok(response);
    }
    readers.windows.remove(&key);
    if let std::collections::btree_map::Entry::Vacant(entry) = readers.tasks.entry(intent) {
        let (send, receiver) = mpsc::channel();
        let directory = journal.directory.clone();
        let input = request.clone();
        let assignment = serde_json::to_value(
            journal
                .data
                .prepared_assignments
                .iter()
                .find(|e| e.launch_intent_id == Some(intent))
                .expect("assignment")
                .assignment
                .decoded(),
        )
        .map_err(|_| failure("invalid log assignment"))?;
        let thread = thread::spawn(move || {
            let response = (|| {
                let _fence = crate::cleanup::read_fence(&directory, intent)?;
                if crate::compute::has_capture_job(&directory, intent) {
                    crate::compute::read_logs(&directory, intent, &input)
                } else {
                    if input.complete {
                        let root = super::local_process::job_directory(&directory, intent);
                        let _ = crate::capture::collect_named(
                            &root,
                            &assignment,
                            &crate::capture::policy(&root)?,
                            true,
                        );
                    }
                    crate::capture::read(
                        &super::local_process::job_directory(&directory, intent),
                        &input,
                    )
                }
            })();
            let _ = send.send(response);
        });
        entry.insert(ReadTask {
            request: request.clone(),
            receiver,
            thread,
        });
    }
    let task = readers.tasks.get(&intent).expect("reader");
    let response = if client.service {
        match task.receiver.try_recv() {
            Ok(value) => Some(value),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(_) => Some(Err(failure("log retrieval worker stopped"))),
        }
    } else {
        Some(
            task.receiver
                .recv_timeout(std::time::Duration::from_secs(35))
                .unwrap_or_else(|_| Err(failure("log retrieval unavailable"))),
        )
    };
    let Some(response) = response else {
        return Err(failure("log retrieval pending").with_kind(super::ErrorKind::Waiting));
    };
    let task = readers.tasks.remove(&intent).expect("reader");
    let matches = task.request.source == request.source
        && task.request.epoch == request.epoch
        && task.request.offset == request.offset
        && task.request.complete == request.complete;
    task.thread
        .join()
        .map_err(|_| failure("log retrieval worker failed"))?;
    if !matches {
        return Err(
            failure("log retrieval cursor changed; retry current position")
                .with_kind(super::ErrorKind::Waiting),
        );
    }
    let window = Window::new(response?, request.complete)?;
    let response = window.chunk(window.response.offset);
    readers.windows.insert(key, window);
    Ok(response)
}

pub fn validate_history(entry: &PreparedAssignment) -> Result<()> {
    let Some(progress) = &entry.logs else {
        return Ok(());
    };
    let invalid =
        || failure("journal contains invalid log progress; preserve it for reconciliation");
    let expected = enumeray_contracts::logs::expected_sources(
        &entry.assignment.decoded().definition,
        entry.assignment.decoded().requirements.driver
            == enumeray_contracts::execution::ExecutionDriver::Slurm,
    );
    if entry.launch_intent_id.is_none()
        || entry.submission_state() == journal::SubmissionState::DefinitelyUnsubmitted
        || progress.streams.len() > expected.len()
        || progress.cursor >= expected.len()
    {
        return Err(invalid());
    }
    for (source, stream) in &progress.streams {
        if !expected.contains(source)
            || stream.epoch < 1
            || stream.next_sequence < 1
            || stream.byte_offset > i64::MAX as u64
            || stream.closed && stream.pending.is_some()
        {
            return Err(invalid());
        }
        if let Some(p) = &stream.pending {
            p.request.validate().map_err(|_| invalid())?;
            if p.request.attempt_id != entry.assignment.decoded().attempt_id
                || p.request.byte_length > CHUNK_BYTES as i64
                || p.byte_offset_after != (p.request.offset + p.request.byte_length) as u64
                || !(p.epoch == stream.epoch && p.sequence == stream.next_sequence
                    || Some(p.epoch) == stream.epoch.checked_add(1) && p.sequence == 1)
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

pub fn forward(
    client: &Client,
    journal: &mut Journal,
    index: usize,
    session: Uuid,
    complete: bool,
) -> Result<()> {
    loop {
        let result = forward_pass(client, journal, index, session, complete);
        match result {
            Err(error)
                if complete && !client.service && error.kind == super::ErrorKind::Waiting =>
            {
                continue;
            }
            other => return other,
        }
    }
}

fn forward_pass(
    client: &Client,
    journal: &mut Journal,
    index: usize,
    session: Uuid,
    complete: bool,
) -> Result<()> {
    let assignment = journal.data.prepared_assignments[index].assignment.clone();
    let intent = journal.data.prepared_assignments[index]
        .launch_intent_id
        .ok_or_else(|| failure("log capture has no launch identity"))?;
    let sources = enumeray_contracts::logs::expected_sources(
        &assignment.decoded().definition,
        assignment.decoded().requirements.driver
            == enumeray_contracts::execution::ExecutionDriver::Slurm,
    );
    if journal.data.prepared_assignments[index].logs.is_none() {
        journal.data.prepared_assignments[index].logs = Some(Progress::default());
        journal.save()?;
    }
    let mut idle = 0;
    for _ in 0..8 {
        let progress = journal.data.prepared_assignments[index]
            .logs
            .as_mut()
            .expect("logs");
        let source = sources[progress.cursor % sources.len()];
        // Keep the cursor on an in-flight read. Advance only after its result
        // or acknowledgement so a completed async request cannot be discarded.
        let stream = progress.streams.entry(source).or_default();
        if stream.closed {
            progress.cursor = (progress.cursor + 1) % sources.len();
            idle += 1;
            if idle == sources.len() {
                return Ok(());
            }
            continue;
        }
        if stream.pending.is_none() {
            let request = ReadLogsRequest {
                source,
                epoch: stream.epoch,
                offset: stream.byte_offset,
                complete,
            };
            let response = read_source(client, journal, intent, request)?;
            if response.epoch < stream_epoch(journal, index, source)
                || response.earliest > response.end
                || response.offset < response.earliest
                || response.offset > response.end
            {
                return Err(failure(
                    "runner capture ranges conflict with delivery history",
                ));
            }
            let stream = journal.data.prepared_assignments[index]
                .logs
                .as_ref()
                .expect("logs")
                .streams
                .get(&source)
                .expect("stream");
            let bytes = STANDARD
                .decode(&response.data)
                .map_err(|_| failure("invalid runner log bytes"))?;
            if bytes.len() > CHUNK_BYTES || response.offset + bytes.len() as u64 > response.end {
                return Err(failure("runner log chunk exceeds bounds"));
            }
            let (epoch, sequence, offset, text, length, gap, closed_after) =
                if response.epoch != stream.epoch {
                    // A source can reset repeatedly before its first read or
                    // while transport is unavailable. Establish the initial
                    // epoch, then acknowledge each lost epoch in order.
                    let (epoch, reason) = reset_delivery(stream)?;
                    (
                        epoch,
                        1,
                        0,
                        String::new(),
                        0,
                        Some(LogGap {
                            reason,
                            bytes: None,
                        }),
                        false,
                    )
                } else if response.offset > stream.byte_offset {
                    (
                        stream.epoch,
                        stream.next_sequence,
                        response.offset,
                        String::new(),
                        0,
                        Some(LogGap {
                            reason: LogGapReason::RetentionDiscarded,
                            bytes: Some((response.offset - stream.byte_offset) as i64),
                        }),
                        false,
                    )
                } else {
                    let (text, consumed) = decode_chunk(
                        &bytes,
                        response.closed && response.offset + bytes.len() as u64 == response.end,
                    );
                    if consumed > 0 {
                        (
                            stream.epoch,
                            stream.next_sequence,
                            response.offset,
                            text,
                            consumed as i64,
                            None,
                            false,
                        )
                    } else if response.closed && response.unavailable {
                        (
                            if stream.next_sequence == 1 {
                                stream.epoch
                            } else {
                                stream
                                    .epoch
                                    .checked_add(1)
                                    .ok_or_else(|| failure("log epoch exhausted"))?
                            },
                            if stream.next_sequence == 1 {
                                stream.next_sequence
                            } else {
                                1
                            },
                            0,
                            String::new(),
                            0,
                            Some(LogGap {
                                reason: LogGapReason::SourceUnavailable,
                                bytes: None,
                            }),
                            true,
                        )
                    } else {
                        let progress = journal.data.prepared_assignments[index]
                            .logs
                            .as_mut()
                            .expect("logs");
                        if response.closed {
                            progress.streams.get_mut(&source).expect("stream").closed = true;
                        }
                        progress.cursor = (progress.cursor + 1) % sources.len();
                        journal.save()?;
                        idle += 1;
                        if idle == sources.len() {
                            break;
                        }
                        continue;
                    }
                };
            let pending = Pending {
                epoch,
                sequence,
                byte_offset_after: offset + length as u64,
                closed_after,
                request: LogBatchRequest {
                    attempt_id: assignment.decoded().attempt_id,
                    offset: offset as i64,
                    byte_length: length,
                    text,
                    gap,
                },
            };
            journal.data.prepared_assignments[index]
                .logs
                .as_mut()
                .expect("logs")
                .streams
                .get_mut(&source)
                .expect("stream")
                .pending = Some(pending);
            journal.save()?;
        }
        let pending = journal.data.prepared_assignments[index]
            .logs
            .as_ref()
            .expect("logs")
            .streams
            .get(&source)
            .expect("stream")
            .pending
            .as_ref()
            .expect("pending")
            .clone();
        let id = assignment.decoded().assignment_id;
        let body = serde_json::to_value(&pending.request)
            .map_err(|_| failure("invalid pending log body"))?;
        let (status, body) = client.request(
            Method::PUT,
            &format!(
                "/connector/v1/assignments/{id}/logs/{}/{}/{}",
                source.as_str(),
                pending.epoch,
                pending.sequence
            ),
            Some(session),
            None,
            Some(&body),
        )?;
        if status != StatusCode::OK {
            return Err(http_error("log batch", status));
        }
        let receipt = parse_json(&body)?;
        if receipt["assignment_id"] != json!(id)
            || receipt["source"] != source.as_str()
            || receipt["epoch"] != pending.epoch
            || receipt["sequence"] != pending.sequence
            || receipt["position"].as_i64().is_none_or(|n| n < 1)
            || !receipt["received_at"].is_string()
        {
            return Err(failure(
                "log acknowledgement conflicts with its durable request",
            ));
        }
        let progress = journal.data.prepared_assignments[index]
            .logs
            .as_mut()
            .expect("logs");
        let stream = progress.streams.get_mut(&source).expect("stream");
        stream.epoch = pending.epoch;
        stream.byte_offset = pending.byte_offset_after;
        stream.next_sequence = pending
            .sequence
            .checked_add(1)
            .ok_or_else(|| failure("log sequence exhausted"))?;
        stream.closed = pending.closed_after;
        stream.pending = None;
        progress.cursor = (progress.cursor + 1) % sources.len();
        idle = 0;
        journal.save()?;
    }
    let closed = journal.data.prepared_assignments[index]
        .logs
        .as_ref()
        .expect("logs")
        .delivered(assignment.decoded());
    if closed {
        client
            .log_readers
            .0
            .lock()
            .expect("log readers")
            .windows
            .retain(|(id, _), _| *id != intent);
    } else if complete {
        return Err(failure("final log delivery pending").with_kind(super::ErrorKind::Waiting));
    }
    Ok(())
}
fn reset_delivery(stream: &Stream) -> Result<(i64, LogGapReason)> {
    if stream.next_sequence == 1 {
        Ok((stream.epoch, LogGapReason::SourceUnavailable))
    } else {
        Ok((
            stream
                .epoch
                .checked_add(1)
                .ok_or_else(|| failure("log epoch exhausted"))?,
            LogGapReason::SourceReset,
        ))
    }
}
fn stream_epoch(journal: &Journal, index: usize, source: LogSource) -> i64 {
    journal.data.prepared_assignments[index]
        .logs
        .as_ref()
        .expect("logs")
        .streams[&source]
        .epoch
}

/// Defer incomplete UTF-8 at a live/chunk boundary. Invalid complete sequences
/// and a final incomplete suffix use U+FFFD. NUL bytes also use U+FFFD because
/// PostgreSQL text excludes NUL; raw capture offsets remain exact.
fn decode_chunk(bytes: &[u8], final_bytes: bool) -> (String, usize) {
    let mut consumed = bytes.len();
    if !final_bytes {
        let mut position = 0;
        while position < bytes.len() {
            match std::str::from_utf8(&bytes[position..]) {
                Ok(_) => break,
                Err(error) => {
                    position += error.valid_up_to();
                    if let Some(length) = error.error_len() {
                        position += length;
                    } else {
                        consumed = position;
                        break;
                    }
                }
            }
        }
    }
    (
        String::from_utf8_lossy(&bytes[..consumed]).replace('\0', "\u{fffd}"),
        consumed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settlement_requires_every_declared_source_even_beyond_one_forwarding_pass() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let mut assignment: enumeray_contracts::assignments::claims::AssignmentReceipt =
            serde_json::from_value(fixture["prepared_assignments"][0]["assignment"].clone())
                .unwrap();
        for n in 0..8 {
            assignment.definition.log_files.insert(
                format!("s{n}"),
                enumeray_contracts::logs::LogFileDeclaration {
                    path: format!("logs/{n}.log"),
                },
            );
        }
        let sources = enumeray_contracts::logs::expected_sources(
            &assignment.definition,
            assignment.requirements.driver == enumeray_contracts::execution::ExecutionDriver::Slurm,
        );
        let mut progress = Progress::default();
        for source in sources.iter().take(8) {
            progress.streams.insert(
                *source,
                Stream {
                    closed: true,
                    ..Default::default()
                },
            );
        }
        assert!(!progress.delivered(&assignment));
        for source in sources {
            progress.streams.insert(
                source,
                Stream {
                    closed: true,
                    ..Default::default()
                },
            );
        }
        assert!(progress.delivered(&assignment));
        let retained = serde_json::to_vec(&progress).unwrap();
        assert!(
            serde_json::from_slice::<Progress>(&retained)
                .unwrap()
                .delivered(&assignment)
        );
    }

    #[test]
    fn resets_before_first_delivery_establish_each_epoch_without_skipping() {
        let mut stream = Stream::default();
        assert_eq!(
            reset_delivery(&stream).unwrap(),
            (1, LogGapReason::SourceUnavailable)
        );
        // Acknowledging an initial unknown loss makes epoch 1 durable even
        // when the first available capture is already epoch 4.
        stream.next_sequence = 2;
        for epoch in 2..=4 {
            assert_eq!(
                reset_delivery(&stream).unwrap(),
                (epoch, LogGapReason::SourceReset)
            );
            stream.epoch = epoch;
        }
        stream.epoch = i64::MAX;
        assert!(reset_delivery(&stream).is_err());
    }

    #[test]
    fn read_ahead_bounds_deliveries_and_refreshes_live_or_final_boundaries() {
        let bytes: Vec<_> = (0..READ_WINDOW_BYTES).map(|n| (n % 251) as u8).collect();
        let window = Window::new(
            ReadLogsResponse {
                epoch: 2,
                earliest: 1000,
                offset: 1000,
                end: 1000 + READ_WINDOW_BYTES as u64,
                data: STANDARD.encode(&bytes),
                closed: false,
                unavailable: false,
            },
            false,
        )
        .unwrap();
        let mut request = ReadLogsRequest {
            source: LogSource::Stdout,
            epoch: 2,
            offset: 0,
            complete: false,
        };
        let mut delivered = Vec::<u8>::new();
        while let Some(chunk) = window.cached(&request) {
            let raw = STANDARD.decode(chunk.data).unwrap();
            assert!(raw.len() <= CHUNK_BYTES);
            delivered.extend(&raw);
            request.offset = chunk.offset + raw.len() as u64;
        }
        assert_eq!(delivered, bytes);
        request.offset -= 2;
        assert!(window.cached(&request).is_none());
        request.offset = 1000;
        request.complete = true;
        assert!(window.cached(&request).is_none());
        request.complete = false;
        request.epoch = 3;
        assert!(window.cached(&request).is_none());
    }

    #[test]
    fn preserves_split_utf8_and_bounds_replacement_expansion() {
        assert_eq!(decode_chunk(b"hello \xf0\x9f", false), ("hello ".into(), 6));
        assert_eq!(
            decode_chunk("\u{1f600}".as_bytes(), false),
            ("\u{1f600}".into(), 4)
        );
        assert_eq!(decode_chunk(b"\xff\xf0\x9f", false), ("\u{fffd}".into(), 1));
        assert_eq!(decode_chunk(b"\xf0\x9f", true), ("\u{fffd}".into(), 2));
        assert_eq!(decode_chunk(b"a\0b", true), ("a\u{fffd}b".into(), 3));
        let (text, consumed) = decode_chunk(&[0xff; CHUNK_BYTES], false);
        assert_eq!(consumed, CHUNK_BYTES);
        assert_eq!(text.len(), CHUNK_BYTES * 3);
    }
}
