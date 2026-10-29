//! Versioned, typed compute-runner requests. Connection credentials never cross this boundary.
use crate::drivers::{CancellationOutcome, CancellationScope};
use crate::{Result, failure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

pub(crate) const VERSION: u32 = 1;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    version: u32,
    action: String,
    payload: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "action",
    content = "payload",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub(crate) enum Operation {
    Validate(crate::compute::ValidationRequest),
    Prepare(crate::compute::ComputeJob),
    Start(Option<()>),
    Status(Option<()>),
    AllocationStatus(Option<()>),
    RequestCancel(CancellationRequest),
    WriteInput(InputChunk),
    WriteCollection(crate::collections::InputPage),
    ReadCollection(crate::collections::OutputPage),
    TransferProbe(TransferProbe),
    ReadOutput(OutputRequest),
    ReadLogs(enumeray_contracts::logs::ReadLogsRequest),
    BuildLogArchive(Option<()>),
    ReadLogArchive(ArchiveReadRequest),
    CleanupLogArchive(Option<()>),
    CleanupRun(crate::cleanup::Request),
    ProtectInstallations(InstallationProtectionRequest),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallationProtectionRequest {
    pub(crate) references: Vec<crate::cleanup::ReleaseReference>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancellationRequest {
    pub(crate) scope: CancellationScope,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancellationResponse {
    pub(crate) cancellation: CancellationOutcome,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputChunk {
    pub(crate) artifact_id: Uuid,
    pub(crate) offset: u64,
    pub(crate) data: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputRequest {
    pub(crate) path: String,
    pub(crate) offset: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArchiveReadRequest {
    pub(crate) offset: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TransferTarget {
    Input { artifact_id: Uuid },
    Output { path: String },
    Archive,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferProbe {
    pub target: TransferTarget,
    pub offset: u64,
    pub length: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferProof {
    pub size_bytes: u64,
    pub checksum_sha256: String,
    pub retained_bytes: u64,
    pub offset: u64,
    pub length: u64,
    pub range_sha256: String,
}
impl Request {
    pub fn new(action: &str, input: Option<&[u8]>) -> Result<Self> {
        let request = Self {
            version: VERSION,
            action: action.into(),
            payload: input
                .map(serde_json::from_slice)
                .transpose()
                .map_err(|_| failure("invalid runner payload"))?
                .unwrap_or(Value::Null),
        };
        request.operation()?;
        Ok(request)
    }
    /// Validate the envelope and keep its typed operation through execution.
    fn operation(&self) -> Result<Operation> {
        if self.version != VERSION {
            return Err(failure(
                "incompatible compute-runner protocol; install matching connector binaries",
            ));
        }
        serde_json::from_value(json!({"action":self.action,"payload":self.payload}))
            .map_err(|_| failure("unsupported runner operation or invalid typed payload"))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    version: u32,
    payload: Value,
}
impl Response {
    pub fn decode(bytes: &[u8]) -> Result<Value> {
        let reply: Self =
            serde_json::from_slice(bytes).map_err(|_| failure("invalid runner response"))?;
        if reply.version != VERSION {
            return Err(failure(
                "incompatible compute-runner protocol; install matching connector binaries",
            ));
        }
        if let Some(proof) = reply.payload.get("transfer") {
            let _: TransferProof = serde_json::from_value(proof.clone())
                .map_err(|_| failure("invalid runner transfer proof"))?;
        } else if reply.payload.get("cancellation").is_some() {
            let _: CancellationResponse = serde_json::from_value(reply.payload.clone())
                .map_err(|_| failure("invalid runner cancellation outcome"))?;
        } else if let Some(status) = reply.payload.get("status") {
            let _: Status = serde_json::from_value(status.clone())
                .map_err(|_| failure("unsupported runner status"))?;
        } else if !reply.payload["data"].is_string() {
            return Err(failure(
                "runner response lacks status, cancellation outcome or output chunk",
            ));
        }
        Ok(reply.payload)
    }
}
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Ready,
    Prepared,
    Staged,
    Pending,
    Running,
    Completed,
    Unresolved,
}

pub(crate) fn run(action: &str, directory: &Path, input: Option<&[u8]>) -> Result<Value> {
    let request: Request =
        serde_json::from_slice(input.ok_or_else(|| failure("runner protocol request required"))?)
            .map_err(|_| failure("invalid versioned runner request"))?;
    let operation = request.operation()?;
    if request.action != action {
        return Err(failure("runner action conflicts with protocol request"));
    }
    let payload = crate::compute::runner(operation, directory)?;
    serde_json::to_value(Response {
        version: VERSION,
        payload,
    })
    .map_err(|_| failure("cannot encode runner response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_v1_runner_wire_remains_decodable() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../release/fixtures/runner-v1.json")).unwrap();
        for request in fixture["requests"].as_array().unwrap() {
            let decoded: Request = serde_json::from_value(request.clone()).unwrap();
            decoded.operation().unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), *request);
        }
        for response in fixture["responses"].as_array().unwrap() {
            assert_eq!(
                Response::decode(&serde_json::to_vec(response).unwrap()).unwrap(),
                response["payload"]
            );
        }
    }

    #[test]
    fn rejects_incompatible_requests_before_touching_compute() {
        for request in [
            json!({"version":2,"action":"status","payload":null}),
            json!({"version":1,"action":"shell","payload":null}),
            json!({"version":1,"action":"start","payload":{"command":"unsafe"}}),
            json!({"version":1,"action":"write-input","payload":{"artifact_id":Uuid::new_v4(),"offset":-1,"data":""}}),
            json!({"version":1,"action":"status","payload":null,"credentials":"unexpected"}),
        ] {
            assert!(
                run(
                    "status",
                    Path::new("/must-not-exist"),
                    Some(&serde_json::to_vec(&request).unwrap())
                )
                .is_err()
            );
        }
        assert!(
            run(
                "start",
                Path::new("/must-not-exist"),
                Some(br#"{"version":1,"action":"status","payload":null}"#)
            )
            .is_err()
        );
        assert!(Request::new("request-cancel", Some(br#"{"scope":"pending"}"#)).is_ok());
        assert!(Request::new("request-cancel", Some(br#"{"scope":"running"}"#)).is_ok());
        for payload in [
            None,
            Some(br#"{"scope":"all"}"#.as_slice()),
            Some(br#"{"scope":"pending","job_id":123}"#.as_slice()),
        ] {
            assert!(Request::new("request-cancel", payload).is_err());
        }
        assert!(Request::new("cancel-pending", None).is_err());
    }
    #[test]
    fn rejects_unknown_response_versions_and_states() {
        for reply in [
            json!({"version":2,"payload":{"status":"ready"}}),
            json!({"version":1,"payload":{"status":"cancelled"}}),
            json!({"version":1,"payload":{}}),
            json!({"version":1,"payload":{"cancellation":{"outcome":"completed"}}}),
            json!({"version":1,"payload":{"cancellation":{"outcome":"unresolved"}}}),
            json!({"version":1,"payload":{"cancellation":{"outcome":"accepted"},"status":"completed"}}),
        ] {
            assert!(Response::decode(&serde_json::to_vec(&reply).unwrap()).is_err());
        }
    }
    #[test]
    fn decodes_explicit_cancellation_outcomes() {
        for cancellation in [
            json!({"outcome":"accepted"}),
            json!({"outcome":"unsupported"}),
            json!({"outcome":"already_terminal"}),
            json!({"outcome":"unresolved","reason":"lost reply"}),
        ] {
            let payload = json!({"cancellation":cancellation});
            assert_eq!(
                Response::decode(
                    &serde_json::to_vec(&json!({"version":1,"payload":payload})).unwrap()
                )
                .unwrap(),
                payload
            );
        }
    }
}
