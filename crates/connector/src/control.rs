//! Self-service enrollment and generation-checked desired configuration.
use crate::{Result, client::Client, compute, failure, journal, local_process};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use enumeray_contracts::execution::*;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MAX_ACK_BYTES: usize = 256 * 1024;
const MAX_ENROLLMENT_BYTES: u64 = 64 * 1024;

#[derive(clap::Args)]
pub struct EnrollOptions {
    #[arg(long)]
    directory: PathBuf,
    #[arg(long)]
    api_base_url: String,
    #[arg(
        long,
        conflicts_with = "enrollment_file",
        required_unless_present = "enrollment_file"
    )]
    enrollment_token_file: Option<PathBuf>,
    #[arg(long)]
    enrollment_file: Option<PathBuf>,
    #[arg(long)]
    allow_insecure_http: bool,
}
#[derive(Serialize, Deserialize)]
struct Identity {
    machine_id: Uuid,
    credential_id: Uuid,
    token: String,
    origin: String,
}

pub fn enroll(options: EnrollOptions) -> Result<Value> {
    let origin =
        crate::client::validate_origin(&options.api_base_url, options.allow_insecure_http)?;
    local_process::create_private_directory(&options.directory)?;
    let identity_path = options.directory.join("identity.json");
    let identity: Identity = if identity_path.exists() {
        local_process::read_record(&identity_path)?
    } else {
        let credential_id = Uuid::new_v4();
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|_| failure("cannot obtain credential entropy"))?;
        let identity = Identity {
            machine_id: Uuid::new_v4(),
            credential_id,
            token: format!("enr_c1_{credential_id}.{}", URL_SAFE_NO_PAD.encode(secret)),
            origin: origin.clone(),
        };
        local_process::write_record(&options.directory, "identity.json", &identity)?;
        identity
    };
    if identity.origin != origin {
        return Err(failure(
            "enrollment directory is bound to another API origin",
        ));
    }
    let source = options
        .enrollment_file
        .as_ref()
        .or(options.enrollment_token_file.as_ref())
        .ok_or_else(|| failure("enrollment credential required"))?;
    let bytes = local_process::read_private(source, MAX_ENROLLMENT_BYTES)?;
    let content =
        String::from_utf8(bytes).map_err(|_| failure("invalid private enrollment credential"))?;
    let enrollment_token = if options.enrollment_file.is_some() {
        let value: Value =
            serde_json::from_str(&content).map_err(|_| failure("invalid enrollment document"))?;
        value["enrollment_token"]
            .as_str()
            .ok_or_else(|| failure("enrollment document lacks enrollment_token"))?
            .to_owned()
    } else {
        content
    };
    if enrollment_token.len() > 512 {
        return Err(failure("enrollment credential exceeds limit"));
    }
    let request = EnrollmentRequest {
        enrollment_token: enrollment_token.trim().to_owned(),
        machine_id: identity.machine_id,
        credential_id: identity.credential_id,
        token_verifier: format!("{:x}", Sha256::digest(identity.token.as_bytes())),
    };
    let http = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| failure("cannot initialize enrollment client"))?;
    let response = http
        .post(format!("{origin}/connector/v1/enroll"))
        .json(&request)
        .send()
        .map_err(|_| {
            failure("enrollment outcome unknown; repeat using the same identity directory")
        })?;
    if !response.status().is_success() {
        return Err(failure(format!(
            "connector enrollment rejected ({})",
            response.status().as_u16()
        )));
    }
    let mut receipt_bytes = Vec::new();
    response
        .take(MAX_ENROLLMENT_BYTES + 1)
        .read_to_end(&mut receipt_bytes)
        .map_err(|_| {
            failure("enrollment receipt unavailable; retry the same identity directory")
        })?;
    if receipt_bytes.len() as u64 > MAX_ENROLLMENT_BYTES {
        return Err(failure(
            "enrollment receipt exceeds limit; retry the same identity directory",
        ));
    }
    let receipt: EnrollmentReceipt = serde_json::from_slice(&receipt_bytes)
        .map_err(|_| failure("invalid enrollment receipt"))?;
    if receipt.credential_id != identity.credential_id {
        return Err(failure(
            "enrollment response conflicts with durable credential identity",
        ));
    }
    let token_path = options.directory.join("token");
    if token_path.exists() {
        if local_process::read_private(&token_path, 256)? != identity.token.as_bytes() {
            return Err(failure("connector token conflicts with identity"));
        }
    } else {
        private_write(&token_path, identity.token.as_bytes())?;
    }
    let receipt_path = options.directory.join("enrollment.json");
    if !receipt_path.exists() {
        local_process::write_record(&options.directory, "enrollment.json", &receipt)?;
    }
    // A prior receipt may already exist when only the token's directory entry
    // was lost. Always make the regenerated token durable before success.
    fs::File::open(&options.directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| failure("cannot persist enrollment directory"))?;
    Ok(
        json!({"status":"enrolled","connector_id":receipt.connector_id,"credential_id":receipt.credential_id,"token_file":token_path}),
    )
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCredentials {
    #[serde(default)]
    pub ssh: BTreeMap<String, PathBuf>,
}

/// Validation happens off the journal owner; only the owner commits the cache.
pub(crate) struct PreparedConfiguration {
    generation: u64,
    validations: Vec<PublicationValidation>,
    installations: Vec<(String, Value)>,
    eligible: std::collections::BTreeSet<(String, String)>,
    dispositions: Vec<Value>,
    desired_bytes: usize,
    execution_contexts: Vec<Value>,
    archived_pins: std::collections::HashSet<(String, String)>,
    pin_revision: Option<String>,
    namespace_reserved: bool,
    release_references: Vec<crate::cleanup::ReleaseReference>,
    work_roots: Vec<String>,
    protection_blocker: Option<String>,
    protection_blocked_bindings: Vec<Value>,
}

/// A durable local commit precedes these HTTP requests. The service hands this
/// back to the configuration worker so slow acknowledgments cannot block work.
pub(crate) struct CommittedConfiguration {
    generation: u64,
    path: PathBuf,
    acknowledgments: Vec<Value>,
}

impl CommittedConfiguration {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Only a successful acknowledgement of a committed generation may enable claims.
pub(crate) struct AcknowledgedConfiguration {
    pub(crate) generation: u64,
    pub(crate) path: PathBuf,
}

fn cache(directory: &Path) -> Result<Value> {
    let path = directory.join("compiled-installations.json");
    let value = if path.exists() {
        local_process::read_record::<Value>(&path)?
    } else {
        json!({"version":1,"generation":0,"installations":[]})
    };
    if value["version"] != 1 || !value["installations"].is_array() {
        return Err(failure(
            "invalid retained installation configuration; preserve the cache",
        ));
    }
    Ok(value)
}

fn binding_key(value: &Value) -> Option<(String, String)> {
    Some((
        value["context_id"].as_str()?.into(),
        value["provider_binding_id"].as_str()?.into(),
    ))
}

pub(crate) fn prepare_configuration(
    client: &Client,
    directory: &Path,
    credentials: Option<&Path>,
) -> Result<PreparedConfiguration> {
    let secrets: HostCredentials = if let Some(path) = credentials {
        serde_json::from_slice(&local_process::read_private(
            path,
            local_process::MAX_RECORD_BYTES,
        )?)
        .map_err(|_| failure("invalid host credential references"))?
    } else {
        HostCredentials::default()
    };
    let cached = cache(directory)?;
    // Maintenance may contract the projection. Refetch after every generation
    // change; never apply a disposition against another desired snapshot.
    for _ in 0..4 {
        let (status, bytes) =
            client.request(Method::GET, "/connector/v1/configuration", None, None, None)?;
        if status != StatusCode::OK {
            return Err(crate::client::http_error("desired configuration", status));
        }
        let desired: DesiredConfiguration =
            serde_json::from_slice(&bytes).map_err(|_| failure("invalid desired configuration"))?;
        let desired_bytes = bytes.len();
        let release_references = desired
            .publications
            .iter()
            .map(|binding| crate::cleanup::ReleaseReference::from_descriptor(&binding.descriptor))
            .collect();
        let work_roots = desired
            .contexts
            .iter()
            .map(|context| context.definition.work_root.clone())
            .collect();
        let wanted: std::collections::BTreeSet<_> = desired
            .publications
            .iter()
            .map(|p| {
                (
                    p.context_id.clone(),
                    p.descriptor.provider_binding_id.clone(),
                )
            })
            .collect();
        let candidates: Vec<_> = cached["installations"]
            .as_array()
            .expect("cache")
            .iter()
            .filter_map(binding_key)
            .filter(|key| !wanted.contains(key))
            .collect();
        let mut eligible = std::collections::BTreeSet::new();
        let mut dispositions = Vec::new();
        let mut changed = false;
        // An empty batch still advances pending retirement requests.
        let batches: Vec<_> = if candidates.is_empty() {
            vec![&candidates[..]]
        } else {
            candidates.chunks(128).collect()
        };
        for batch in batches {
            let body = json!({"bindings":batch.iter().map(|(context_id, provider_binding_id)|
                json!({"context_id":context_id,"provider_binding_id":provider_binding_id})).collect::<Vec<_>>()});
            let (status, bytes) = client.request(
                Method::POST,
                &format!(
                    "/connector/v1/configuration/{}/retention-dispositions",
                    desired.generation
                ),
                None,
                None,
                Some(&body),
            )?;
            // Older services cannot authorize pruning; ordinary validation can
            // continue while every cached binding is conservatively retained.
            if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
                break;
            }
            if status != StatusCode::OK {
                return Err(crate::client::http_error("configuration retention", status));
            }
            let receipt: ConfigurationRetentionReceipt = serde_json::from_slice(&bytes)
                .map_err(|_| failure("invalid configuration retention receipt"))?;
            if receipt.configuration_changed || receipt.generation != desired.generation {
                changed = true;
                break;
            }
            for disposition in receipt.dispositions {
                let key = (
                    disposition.context_id.clone(),
                    disposition.provider_binding_id.clone(),
                );
                if !batch.contains(&key) {
                    return Err(failure(
                        "retention disposition conflicts with requested identity",
                    ));
                }
                if disposition.state == InstallationDispositionState::Eligible {
                    eligible.insert(key);
                }
                dispositions.push(
                    serde_json::to_value(disposition)
                        .map_err(|_| failure("invalid retention disposition"))?,
                );
            }
        }
        if changed {
            continue;
        }
        // Seed canonical release pins on the actual hosts before cached or
        // unready references can disappear from the retained projection. A lost
        // host reply retains configuration and payloads; validation can continue.
        let protection = seed_release_protection(&desired, &cached, &secrets);
        let protection_seeded = protection.blockers.is_empty();
        let protection_blocker = (!protection_seeded).then(|| protection.blockers.join("; "));
        // A failed host cannot authorize removal of its own references, but its
        // failure does not revoke protection successfully established elsewhere.
        eligible.retain(|key| !protection.blocked_bindings.contains_key(key));
        let protection_blocked_bindings = protection
            .blocked_bindings
            .into_iter()
            .map(|((context_id, provider_binding_id), diagnostic)| {
                json!({"context_id":context_id,"provider_binding_id":provider_binding_id,"diagnostic":diagnostic})
            })
            .collect();
        let mut installations = Vec::new();
        let mut validations = Vec::new();
        for binding in desired.publications {
            let result = desired
                .contexts
                .iter()
                .find(|context| context.id == binding.context_id)
                .ok_or_else(|| failure("execution context is absent"))
                .and_then(|context| {
                    check_requirements(context, &binding.requirements)?;
                    compile(
                        context,
                        &binding.descriptor,
                        &secrets,
                        client.allow_development_execution,
                    )
                });
            let ready = result.is_ok();
            let message = result.as_ref().err().map(ToString::to_string);
            let mut verified_digests = BTreeMap::new();
            if let Ok(value) = result {
                let pinned: BTreeMap<String, String> =
                    serde_json::from_value(value["installation"]["file_digests"].clone())
                        .map_err(|_| failure("validated installation lacks digest evidence"))?;
                for (path, digest) in pinned {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(digest)
                        .map_err(|_| {
                            failure("validated installation digest encoding is invalid")
                        })?;
                    if bytes.len() != 32 {
                        return Err(failure("validated installation digest is not SHA256"));
                    }
                    verified_digests.insert(
                        path,
                        bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
                    );
                }
                installations.push((binding.publication_id.clone(), value));
            }
            validations.push(PublicationValidation {
                publication_id: binding.publication_id,
                generation: binding.generation,
                ready,
                message,
                verified_digests,
            });
        }
        let before = journal_revision(directory)?;
        let archived_pins = if eligible.is_empty() {
            Ok(std::collections::HashSet::new())
        } else {
            crate::cleanup::retained_binding_keys(directory)
        };
        let after = journal_revision(directory)?;
        let stable_pins = archived_pins.is_ok() && before.is_some() && before == after;
        return Ok(PreparedConfiguration {
            generation: desired.generation,
            execution_contexts: desired.contexts.iter().map(|context| json!({"context_id":context.id,"execution_mode":context.definition.execution_mode,"warning":(context.definition.execution_mode == ExecutionMode::Development).then_some(crate::execution_policy::WARNING)})).collect(),
            validations,
            installations,
            eligible,
            dispositions,
            desired_bytes,
            archived_pins: archived_pins.unwrap_or_default(),
            pin_revision: stable_pins.then_some(before).flatten(),
            namespace_reserved: desired.run_payload_namespace_reserved && protection_seeded,
            release_references,
            work_roots,
            protection_blocker,
            protection_blocked_bindings,
        });
    }
    Err(
        failure("configuration retirement advanced; refetch before committing the cache")
            .with_kind(crate::ErrorKind::Retry),
    )
}

/// Must run while owning the journal. Recheck local pins at commit, since
/// assignments may have arrived while the background worker validated files.
pub(crate) fn apply_configuration(
    directory: &Path,
    mut prepared: PreparedConfiguration,
) -> Result<CommittedConfiguration> {
    // A cleanup pass may already hold an older reference snapshot. Publish
    // local protection and prune only after that deletion pass releases its
    // shared fence; HTTP acknowledgment begins after this guard is dropped.
    let _configuration = crate::cleanup::configuration_fence(directory, false)?;
    let mut merged = cache(directory)?;
    if merged["generation"].as_u64().unwrap_or(0) > prepared.generation {
        return Err(failure(
            "stale configuration cannot replace a newer installation cache",
        ));
    }
    // Preserve every known descriptor, including candidates that did not pass
    // validation and manual/older cached entries. Unknown cache shapes cannot
    // certify payload deletion, but do not stop scientific execution.
    for entry in merged["installations"].as_array().expect("cache") {
        let descriptor =
            serde_json::from_value::<InstallationDescriptor>(entry["descriptor"].clone());
        if let (Ok(descriptor), Some(work_root)) = (descriptor, entry["work_root"].as_str()) {
            prepared
                .release_references
                .push(crate::cleanup::ReleaseReference::from_descriptor(
                    &descriptor,
                ));
            prepared.work_roots.push(work_root.to_owned());
        } else {
            prepared.namespace_reserved = false;
        }
    }
    let journal_path = directory.join("journal.json");
    let active_keys: std::collections::BTreeSet<_> = if journal_path.exists() {
        let bytes = local_process::read_private(&journal_path, 16 * 1024 * 1024)?;
        let data: journal::JournalData =
            serde_json::from_slice(&bytes).map_err(|_| failure("invalid assignment pins"))?;
        for entry in &data.prepared_assignments {
            prepared
                .release_references
                .push(crate::cleanup::ReleaseReference::from_descriptor(
                    &entry.assignment.decoded().descriptor,
                ));
        }
        data.prepared_assignments
            .iter()
            .map(|entry| {
                (
                    entry.assignment["context_id"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                    entry.assignment["descriptor"]["provider_binding_id"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                )
            })
            .collect()
    } else {
        std::collections::BTreeSet::new()
    };
    // Host seeds protect compute payloads; the connector must also durably pin
    // known local aliases before dropping any cached descriptor. Preserve those
    // pins even while an unrelated host keeps global payload cleanup disabled.
    if let Err(error) = crate::cleanup::retain_release_references(
        directory,
        &prepared.work_roots,
        &prepared.release_references,
    ) {
        prepared.namespace_reserved = false;
        prepared.protection_blocker = Some(match prepared.protection_blocker {
            Some(prior) => format!("{prior}; {}", error.message),
            None => error.message.clone(),
        });
        prepared.protection_blocked_bindings.extend(prepared.eligible.iter().map(|(context_id, provider_binding_id)|
            json!({"context_id":context_id,"provider_binding_id":provider_binding_id,"diagnostic":error.message})));
        prepared.eligible.clear();
    }
    let inventory_current =
        prepared.pin_revision.is_some() && prepared.pin_revision == journal_revision(directory)?;
    let mut local_pins = Vec::new();
    let mut pruned = 0;
    let mut retained = Vec::new();
    for entry in merged["installations"].as_array().expect("cache") {
        let remove = if let Some(key) =
            binding_key(entry).filter(|key| prepared.eligible.contains(key))
        {
            if !inventory_current
                || active_keys.contains(&key)
                || prepared.archived_pins.contains(&key)
            {
                local_pins.push(json!({"context_id":key.0,"provider_binding_id":key.1,"blocker":"local_work_or_cleanup_pinned"}));
                false
            } else {
                true
            }
        } else {
            false
        };
        if remove {
            pruned += 1;
        } else {
            retained.push(entry.clone());
        }
    }
    merged["generation"] = json!(prepared.generation);
    merged["installations"] = json!(retained);
    let mut retained_bytes = serde_json::to_vec(&merged)
        .map_err(|_| failure("cannot encode retained configuration"))?
        .len();
    for (publication_id, value) in prepared.installations {
        let entries = merged["installations"].as_array_mut().expect("cache");
        let problem = if let Some(prior) = entries
            .iter()
            .find(|entry| entry["provider_binding_id"] == value["provider_binding_id"])
        {
            (prior != &value).then(|| {
                "immutable installation changed under an existing provider binding identity"
                    .to_owned()
            })
        } else {
            retain_installation(entries, value.clone(), &mut retained_bytes)
                .err()
                .map(|e| e.message)
        };
        if let Some(message) = problem {
            for validation in &mut prepared.validations {
                if validation.publication_id == publication_id {
                    validation.ready = false;
                    validation.message = Some(message.clone());
                }
            }
        }
    }
    let encoded =
        serde_json::to_vec(&merged).map_err(|_| failure("cannot encode compiled configuration"))?;
    if encoded.len() > local_process::MAX_RECORD_BYTES as usize {
        return Err(failure(
            "retained configuration exceeds the 4 MiB connector limit",
        ));
    }
    atomic_write(directory, "compiled-installations.json", &encoded)?;
    let release_protection = crate::cleanup::record_release_protection(
        directory,
        prepared.generation,
        prepared.namespace_reserved,
        &prepared.work_roots,
        &prepared.release_references,
    );
    let diagnostics = json!({"execution_contexts":prepared.execution_contexts,"generation":prepared.generation,"desired_bytes":prepared.desired_bytes,"desired_limit_bytes":2*1024*1024,
        "cache_bytes":encoded.len(),"cache_limit_bytes":local_process::MAX_RECORD_BYTES,"cached_bindings":merged["installations"].as_array().expect("cache").len(),
        "pruned_bindings":pruned,"dispositions":prepared.dispositions,"local_pins":local_pins,
        "release_protection_ready":release_protection.is_ok(),"release_protection_blocker":prepared.protection_blocker.or_else(||release_protection.err().map(|error| error.message)),
        "protection_blocked_bindings":prepared.protection_blocked_bindings});
    atomic_write(
        directory,
        "configuration-retention.json",
        &serde_json::to_vec(&diagnostics)
            .map_err(|_| failure("cannot encode retention diagnostics"))?,
    )?;
    Ok(CommittedConfiguration {
        generation: prepared.generation,
        path: directory.join("compiled-installations.json"),
        acknowledgments: acknowledgment_batches(prepared.validations)?,
    })
}

pub(crate) fn acknowledge_configuration(
    client: &Client,
    committed: CommittedConfiguration,
) -> Result<AcknowledgedConfiguration> {
    for request in committed.acknowledgments {
        let (status, _) = client.request(
            Method::PUT,
            &format!("/connector/v1/configuration/{}/ack", committed.generation),
            None,
            None,
            Some(&request),
        )?;
        if status != StatusCode::OK {
            return Err(crate::client::http_error(
                "configuration acknowledgment",
                status,
            ));
        }
    }
    Ok(AcknowledgedConfiguration {
        generation: committed.generation,
        path: committed.path,
    })
}

fn journal_revision(directory: &Path) -> Result<Option<String>> {
    let path = directory.join("journal.json");
    if !path
        .try_exists()
        .map_err(|_| failure("cannot inspect journal revision"))?
    {
        return Ok(None);
    }
    let bytes = local_process::read_private(&path, 16 * 1024 * 1024)?;
    Ok(Some(format!("{:x}", Sha256::digest(bytes))))
}

pub fn reconcile(client: &Client, directory: &Path, credentials: Option<&Path>) -> Result<PathBuf> {
    let prepared = prepare_configuration(client, directory, credentials)?;
    let committed = apply_configuration(directory, prepared)?;
    Ok(acknowledge_configuration(client, committed)?.path)
}

pub(crate) fn summary(directory: &Path) -> Result<Value> {
    let path = directory.join("configuration-retention.json");
    if path.exists() {
        local_process::read_record(&path)
    } else {
        let value = cache(directory)?;
        Ok(
            json!({"generation":value["generation"],"cache_bytes":serde_json::to_vec(&value).map_err(|_| failure("cannot encode cache"))?.len(),
            "cache_limit_bytes":local_process::MAX_RECORD_BYTES,"cached_bindings":value["installations"].as_array().expect("cache").len()}),
        )
    }
}

// Retain earlier installations while accepted work still references them. Refuse
// an addition before it can make the durable cache unreadable on the next poll.
fn retain_installation(entries: &mut Vec<Value>, value: Value, bytes: &mut usize) -> Result<()> {
    let addition = serde_json::to_vec(&value)
        .map_err(|_| failure("cannot encode retained installation"))?
        .len()
        + usize::from(!entries.is_empty());
    if bytes.saturating_add(addition) > local_process::MAX_RECORD_BYTES as usize {
        return Err(failure(
            "retained installation configuration reached the 4 MiB connector limit; retire obsolete publication configuration and finish pending payload cleanup, or use another connector",
        ));
    }
    *bytes += addition;
    entries.push(value);
    Ok(())
}

fn acknowledgment_batches(validations: Vec<PublicationValidation>) -> Result<Vec<Value>> {
    let envelope_bytes = serde_json::to_vec(&json!({"validations":[]}))
        .map_err(|_| failure("cannot encode validation report"))?
        .len();
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut bytes = envelope_bytes;
    for validation in validations {
        let value = serde_json::to_value(validation)
            .map_err(|_| failure("cannot encode validation report"))?;
        let length = serde_json::to_vec(&value)
            .map_err(|_| failure("cannot encode validation report"))?
            .len();
        if envelope_bytes + length > MAX_ACK_BYTES {
            return Err(failure(format!(
                "validation report for publication {} exceeds the 256 KiB acknowledgment limit; reduce descriptor or diagnostic size",
                value["publication_id"].as_str().unwrap_or("unknown")
            )));
        }
        let separator = usize::from(!current.is_empty());
        if bytes + length + separator > MAX_ACK_BYTES {
            batches.push(json!({"validations":current}));
            current = Vec::new();
            bytes = envelope_bytes;
        }
        bytes += length + usize::from(!current.is_empty());
        current.push(value);
    }
    if !current.is_empty() || batches.is_empty() {
        batches.push(json!({"validations":current}));
    }
    Ok(batches)
}

fn check_requirements(
    context: &DesiredContext,
    requirements: &ExecutionRequirements,
) -> Result<()> {
    if requirements.driver != context.definition.driver.kind()
        || requirements.wrapper_protocol_version != 1
        || requirements.capabilities.iter().any(|capability| {
            !matches!(
                capability,
                Capability::JsonInputs
                    | Capability::JsonOutputs
                    | Capability::ArtifactInputs
                    | Capability::ArtifactOutputs
                    | Capability::LargeArtifacts
                    | Capability::CollectionInputs
                    | Capability::CollectionOutputs
            )
        })
    {
        return Err(failure(
            "connector does not support publication execution requirements",
        ));
    }
    Ok(())
}

fn compile(
    desired_context: &DesiredContext,
    descriptor: &InstallationDescriptor,
    credentials: &HostCredentials,
    allow_development_execution: bool,
) -> Result<Value> {
    let context = &desired_context.definition;
    crate::execution_policy::require_consent(context.execution_mode, allow_development_execution)?;
    validate_descriptor(context, descriptor)?;
    context.validate_execution().map_err(failure)?;
    let mut remote = transport_binding(desired_context, descriptor, credentials)?;
    remote.installation.file_digests =
        compute::validate_context(&remote, context, descriptor, allow_development_execution)?;
    serde_json::to_value(remote).map_err(|_| failure("invalid execution installation"))
}

fn transport_binding(
    desired_context: &DesiredContext,
    descriptor: &InstallationDescriptor,
    credentials: &HostCredentials,
) -> Result<compute::ComputeInstallation> {
    let context = &desired_context.definition;
    let entry = descriptor
        .entrypoint
        .first()
        .ok_or_else(|| failure("installation entrypoint is absent"))?;
    let executable = Path::new(&descriptor.release_path).join(entry);
    let mut argv = vec![executable.to_string_lossy().into_owned()];
    argv.extend_from_slice(&descriptor.entrypoint[1..]);
    let identity_file = match &context.connection {
        ConnectionConfig::Ssh(ssh) => credentials.ssh.get(&ssh.credential_reference).cloned(),
        ConnectionConfig::Direct { .. } => None,
    };
    Ok(compute::ComputeInstallation {
        execution_mode: context.execution_mode,
        descriptor: descriptor.clone(),
        provider_binding_id: descriptor.provider_binding_id.clone(),
        context_id: desired_context.id.clone(),
        installation: local_process::Installation {
            provider_binding_id: descriptor.provider_binding_id.clone(),
            installation: descriptor.installation.clone(),
            argv,
            environment: descriptor.environment.clone(),
            file_digests: Default::default(),
        },
        work_root: context.work_root.clone(),
        shared_workspace: context.shared_workspace.clone(),
        connection: context.connection.clone(),
        identity_file,
        driver: context.driver.clone(),
        slurm_script: descriptor.slurm_script.as_ref().map(|script| {
            Path::new(&descriptor.release_path)
                .join(script)
                .to_string_lossy()
                .into_owned()
        }),
    })
}

#[derive(Default)]
struct ProtectionSeed {
    blocked_bindings: BTreeMap<(String, String), String>,
    blockers: Vec<String>,
}

fn seed_release_protection(
    desired: &DesiredConfiguration,
    cached: &Value,
    credentials: &HostCredentials,
) -> ProtectionSeed {
    struct Host {
        binding: compute::ComputeInstallation,
        references: Vec<crate::cleanup::ReleaseReference>,
        keys: Vec<(String, String)>,
    }
    let mut hosts: BTreeMap<String, Host> = BTreeMap::new();
    let mut outcome = ProtectionSeed::default();
    let mut insert = |binding: compute::ComputeInstallation| -> Result<()> {
        let key = serde_json::to_string(&(&binding.connection, &binding.identity_file))
            .map_err(|_| failure("cannot identify installation protection transport"))?;
        let reference = crate::cleanup::ReleaseReference::from_descriptor(&binding.descriptor);
        let binding_key = (
            binding.context_id.clone(),
            binding.provider_binding_id.clone(),
        );
        let host = hosts.entry(key).or_insert_with(|| Host {
            binding,
            references: Vec::new(),
            keys: Vec::new(),
        });
        host.references.push(reference);
        host.keys.push(binding_key);
        Ok(())
    };
    for entry in cached["installations"].as_array().expect("cache") {
        let result = serde_json::from_value(entry.clone())
            .map_err(|_| failure("manual installation metadata cannot certify payload cleanup"))
            .and_then(&mut insert);
        if let Err(error) = result {
            if let Some(key) = binding_key(entry) {
                outcome.blocked_bindings.insert(key, error.message.clone());
            }
            outcome.blockers.push(error.message);
        }
    }
    for publication in &desired.publications {
        let result = desired
            .contexts
            .iter()
            .find(|context| context.id == publication.context_id)
            .ok_or_else(|| failure("installation protection context is absent"))
            .and_then(|context| transport_binding(context, &publication.descriptor, credentials))
            .and_then(&mut insert);
        if let Err(error) = result {
            outcome.blocked_bindings.insert(
                (
                    publication.context_id.clone(),
                    publication.descriptor.provider_binding_id.clone(),
                ),
                error.message.clone(),
            );
            outcome.blockers.push(error.message);
        }
    }
    for (_, mut host) in hosts {
        host.references.sort();
        host.references.dedup();
        let result = host
            .references
            .chunks(128)
            .try_for_each(|batch| compute::protect_installations(&host.binding, batch));
        if let Err(error) = result {
            let transport = match &host.binding.connection {
                ConnectionConfig::Direct { .. } => "direct execution host".to_owned(),
                ConnectionConfig::Ssh(target) => {
                    format!("SSH host {}:{}", target.host, target.port)
                }
            };
            let diagnostic = format!("{transport}: {}", error.message);
            for key in host.keys {
                outcome.blocked_bindings.insert(key, diagnostic.clone());
            }
            outcome.blockers.push(diagnostic);
        }
    }
    outcome.blockers.sort();
    outcome.blockers.dedup();
    outcome
}

pub(crate) fn validate_descriptor(
    context: &ExecutionContextDefinition,
    descriptor: &InstallationDescriptor,
) -> Result<()> {
    descriptor
        .validate_driver(context.driver.kind())
        .map_err(failure)?;
    context
        .driver
        .validate_environment(&descriptor.environment)
        .map_err(failure)?;
    if descriptor.installation.wrapper_protocol_version != 1
        || descriptor.entrypoint.is_empty()
        || descriptor.entrypoint.len() > 32
    {
        return Err(failure("descriptor exceeds context capabilities"));
    }
    let release = Path::new(&descriptor.release_path);
    if !release.is_absolute()
        || release
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        || !context
            .installation_roots
            .iter()
            .any(|root| release.starts_with(root) && release != Path::new(root))
    {
        return Err(failure(
            "release-specific installation must be beneath an approved root",
        ));
    }
    let entry = Path::new(&descriptor.entrypoint[0]);
    if entry.is_absolute()
        || entry
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        || descriptor
            .entrypoint
            .iter()
            .any(|s| s.contains('\0') || s.len() > 4096)
    {
        return Err(failure(
            "entrypoint must be relative to its immutable release directory",
        ));
    }
    Ok(())
}

pub(crate) fn check_release(
    installation_roots: &[String],
    descriptor: &InstallationDescriptor,
) -> Result<()> {
    let release = fs::canonicalize(&descriptor.release_path)
        .map_err(|_| failure("release directory is not installed"))?;
    if !installation_roots
        .iter()
        .filter_map(|p| fs::canonicalize(p).ok())
        .any(|root| release.starts_with(&root) && release != root)
    {
        return Err(failure(
            "release directory resolves outside approved installation roots",
        ));
    }
    let executable = fs::canonicalize(release.join(&descriptor.entrypoint[0]))
        .map_err(|_| failure("entrypoint is not installed"))?;
    if !executable.starts_with(&release) || !executable.is_file() {
        return Err(failure("entrypoint escapes immutable release directory"));
    }
    if let Some(script) = &descriptor.slurm_script {
        let script = fs::canonicalize(release.join(script))
            .map_err(|_| failure("Slurm batch file is not installed"))?;
        if !script.starts_with(&release) || !script.is_file() {
            return Err(failure(
                "Slurm batch file escapes immutable release directory",
            ));
        }
        validate_slurm_script(&script)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&executable)
            .map_err(|_| failure("entrypoint unavailable"))?
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Err(failure("installed entrypoint is not executable"));
        }
    }
    Ok(())
}

fn validate_slurm_script(path: &Path) -> Result<()> {
    // Readiness and launch must enforce the same script contract.
    crate::drivers::slurm::read_batch_file(path).map(|_| ())
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| failure("cannot create private connector file"))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("cannot persist private connector file"))
}
fn atomic_write(directory: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    private_write(&temporary, bytes)?;
    fs::rename(&temporary, directory.join(name))
        .map_err(|_| failure("cannot publish compiled configuration"))?;
    fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| failure("cannot persist compiled configuration"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> ExecutionContextDefinition {
        ExecutionContextDefinition {
            execution_mode: Default::default(),
            driver: DriverConfig::Process { scratch: None },
            installation_roots: vec!["/opt/models".into()],
            work_root: "/srv/jobs".into(),
            connection: ConnectionConfig::Direct {
                launcher: vec!["/usr/local/libexec/isolated-model".into()],
            },
            max_outstanding_assignments: 2,
            shared_workspace: None,
        }
    }
    fn descriptor() -> InstallationDescriptor {
        InstallationDescriptor {
            provider_binding_id: "release-1".into(),
            installation: InstallationIdentity {
                model_identity: "model-1".into(),
                wrapper_identity: "wrapper-1".into(),
                runtime_identity: "runtime-1".into(),
                wrapper_protocol_version: 1,
                dataset_identities: vec![],
            },
            release_path: "/opt/models/release-1".into(),
            entrypoint: vec!["wrapper".into(), "execute".into()],
            slurm_script: None,
            environment: BTreeMap::new(),
        }
    }
    #[test]
    fn derived_requirements_do_not_claim_connector_support() {
        let context: DesiredContext=serde_json::from_value(serde_json::json!({"id":"test", "definition":{
            "driver":{"kind":"process"},"connection":{"kind":"direct","launcher":["/bin/runner"]},
            "installation_roots":["/opt/models"],"work_root":"/srv/jobs","max_outstanding_assignments":1
        }})).unwrap();
        let mut required = ExecutionRequirements {
            driver: ExecutionDriver::Process,
            capabilities: vec![Capability::JsonInputs, Capability::JsonOutputs],
            wrapper_protocol_version: 1,
        };
        check_requirements(&context, &required).unwrap();
        required.wrapper_protocol_version = 2;
        assert!(check_requirements(&context, &required).is_err());
        required.wrapper_protocol_version = 1;
        required.driver = ExecutionDriver::Slurm;
        assert!(check_requirements(&context, &required).is_err());
    }
    #[test]
    fn acknowledgments_are_bounded_and_preserve_every_generation() {
        let report = |index| PublicationValidation {
            publication_id: format!("publication-{index}"),
            generation: index,
            ready: false,
            message: Some("diagnostic".repeat(4096)),
            verified_digests: BTreeMap::new(),
        };
        let batches = acknowledgment_batches((0..20).map(report).collect()).unwrap();
        assert!(batches.len() > 1);
        let reports: Vec<_> = batches
            .iter()
            .flat_map(|batch| {
                assert!(serde_json::to_vec(batch).unwrap().len() <= MAX_ACK_BYTES);
                batch["validations"].as_array().unwrap().iter()
            })
            .collect();
        assert_eq!(reports.len(), 20);
        for (index, report) in reports.iter().enumerate() {
            assert_eq!(report["generation"], index);
        }
        let mut oversized = report(1);
        oversized.message = Some("x".repeat(MAX_ACK_BYTES));
        assert!(acknowledgment_batches(vec![oversized]).is_err());
        assert_eq!(
            acknowledgment_batches(vec![]).unwrap(),
            vec![json!({"validations":[]})]
        );
    }

    #[test]
    fn retained_cache_rejects_growth_without_losing_usable_installations() {
        let prior = json!({"provider_binding_id":"existing"});
        let mut entries = vec![prior.clone()];
        let mut bytes = local_process::MAX_RECORD_BYTES as usize - 8;
        let original_bytes = bytes;
        assert!(
            retain_installation(
                &mut entries,
                json!({"provider_binding_id":"new"}),
                &mut bytes
            )
            .is_err()
        );
        assert_eq!(entries, vec![prior]);
        assert_eq!(bytes, original_bytes);
        bytes = 100;
        retain_installation(
            &mut entries,
            json!({"provider_binding_id":"new"}),
            &mut bytes,
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn delegation_requires_confined_releases_and_driver_specific_scripts() {
        let context = context();
        let mut descriptor = descriptor();
        assert!(validate_descriptor(&context, &descriptor).is_ok());
        let mut unsupported = context.clone();
        unsupported.driver = DriverConfig::Slurm(SlurmConfig {
            worker_path: "/bin/runner".into(),
        });
        assert!(validate_descriptor(&unsupported, &descriptor).is_err());
        descriptor.slurm_script = Some("model.sbatch".into());
        assert!(validate_descriptor(&unsupported, &descriptor).is_ok());
        descriptor.slurm_script = None;
        for path in ["/opt/models", "/opt/models/../private", "/etc/private"] {
            descriptor.release_path = path.into();
            assert!(validate_descriptor(&context, &descriptor).is_err());
        }
        descriptor.release_path = "/opt/models/release-1".into();
        descriptor.entrypoint[0] = "../../bin/sh".into();
        assert!(validate_descriptor(&context, &descriptor).is_err());
        descriptor.entrypoint[0] = "wrapper".into();
        descriptor
            .environment
            .insert("LD_PRELOAD".into(), "/tmp/inject.so".into());
        assert!(validate_descriptor(&context, &descriptor).is_err());
    }

    #[test]
    fn process_path_environment_is_reserved_only_for_configured_scratch() {
        let mut configured = context();
        let manifest: Value = serde_json::from_str(include_str!(
            "../../cli/examples/process-direct-context.json"
        ))
        .unwrap();
        configured.driver =
            serde_json::from_value(manifest["spec"]["definition"]["driver"].clone()).unwrap();
        for key in ["TMPDIR", "TMP", "TEMP", "XDG_CACHE_HOME"] {
            let mut descriptor = descriptor();
            descriptor
                .environment
                .insert(key.into(), "/site/tmp".into());
            assert!(validate_descriptor(&configured, &descriptor).is_err());
            validate_descriptor(&context(), &descriptor).unwrap();
        }
    }

    #[test]
    fn descriptor_environment_protects_runtime_keys_for_both_drivers() {
        for driver in [
            DriverConfig::Process { scratch: None },
            DriverConfig::Slurm(SlurmConfig {
                worker_path: "/bin/runner".into(),
            }),
        ] {
            let mut context = context();
            context.driver = driver;
            for key in [
                "HOME",
                "PATH",
                "PYTHONDONTWRITEBYTECODE",
                "SLURM_JOB_ID",
                "PMI_RANK",
                "PMIX_NAMESPACE",
                "LD_PRELOAD",
                "DYLD_LIBRARY_PATH",
            ] {
                let mut descriptor = descriptor();
                descriptor.slurm_script =
                    matches!(context.driver, DriverConfig::Slurm(_)).then(|| "model.sbatch".into());
                descriptor
                    .environment
                    .insert(key.into(), "untrusted-value".into());
                let error = validate_descriptor(&context, &descriptor).unwrap_err();
                assert!(error.message.contains(key));
                assert!(!error.message.contains("untrusted-value"));
                descriptor.environment.clear();
                descriptor
                    .environment
                    .insert("DATASET_ROOT".into(), "/data/release-1".into());
                validate_descriptor(&context, &descriptor).unwrap();
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn installed_slurm_files_are_readable_and_cannot_escape_the_release() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = std::env::temp_dir().join(format!("enumeray-slurm-release-{}", Uuid::new_v4()));
        let release = root.join("release-1");
        fs::create_dir_all(&release).unwrap();
        fs::write(release.join("wrapper"), b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(release.join("wrapper"), fs::Permissions::from_mode(0o755)).unwrap();
        let mut descriptor = descriptor();
        descriptor.release_path = release.to_string_lossy().into_owned();
        descriptor.slurm_script = Some("model.sbatch".into());
        let roots = vec![root.to_string_lossy().into_owned()];
        assert!(check_release(&roots, &descriptor).is_err());
        // sbatch reads scripts; a batch file need not have executable permission.
        let script = release.join("model.sbatch");
        fs::write(
            &script,
            b"#!/bin/sh\n#SBATCH --gres=gpu:1\nexec \"$1\" local-worker --directory \"$2\"\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();
        check_release(&roots, &descriptor).unwrap();
        for invalid in [
            b"no shebang\n".as_slice(),
            b"#!/bin/sh\r\n",
            b"#!/bin/sh\n\0",
            b"#!/bin/sh\n\xff",
        ] {
            fs::write(&script, invalid).unwrap();
            assert!(check_release(&roots, &descriptor).is_err());
        }
        fs::remove_file(&script).unwrap();
        fs::write(root.join("outside.sbatch"), b"#!/bin/sh\nexit 0\n").unwrap();
        symlink(root.join("outside.sbatch"), &script).unwrap();
        assert!(check_release(&roots, &descriptor).is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(all(test, unix))]
mod retention_tests {
    use super::*;
    use axum::{
        Json, Router,
        body::{Body, to_bytes},
        extract::State,
        http::Request,
        response::{IntoResponse, Response},
    };
    use std::sync::{Arc, Mutex, mpsc};

    #[derive(Clone, Copy)]
    enum Disposition {
        Eligible,
        Pinned,
        Unmanaged,
        Missing,
        Stale,
        Changed,
    }

    struct Peer {
        mode: Disposition,
        generation: u64,
        requests: Vec<(String, String, Value)>,
        configuration: Value,
    }

    struct Fixture {
        directory: PathBuf,
        origin: String,
        peer: Arc<Mutex<Peer>>,
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Fixture {
        fn new(mode: Disposition) -> Self {
            let directory = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("enumeray-config-retention-{}", Uuid::new_v4()));
            local_process::create_private_directory(&directory).unwrap();
            let peer = Arc::new(Mutex::new(Peer {
                mode,
                generation: 1,
                requests: Vec::new(),
                configuration: json!({"contexts":[],"publications":[],"run_payload_namespace_reserved":true}),
            }));
            let state = peer.clone();
            let (origin_send, origin_receive) = mpsc::channel();
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let worker = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async move {
                        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                        origin_send
                            .send(format!("http://{}", listener.local_addr().unwrap()))
                            .unwrap();
                        axum::serve(listener, Router::new().fallback(handle).with_state(state))
                            .with_graceful_shutdown(async {
                                let _ = stopped.await;
                            })
                            .await
                            .unwrap();
                    });
            });
            let origin = origin_receive.recv().unwrap();
            private_write(&directory.join("token"), b"test-retention-credential").unwrap();
            let fixture = Self {
                directory,
                origin,
                peer,
                stop: Some(stop),
                worker: Some(worker),
            };
            fixture.journal(None);
            fixture.cache(1, vec![binding("test-context", "scalar-double-r1")]);
            fixture
        }

        fn client(&self) -> Client {
            Client::configured(&self.origin, Some(&self.directory.join("token")), false).unwrap()
        }

        fn journal(&self, entry: Option<Value>) {
            let mut document: Value = serde_json::from_str(include_str!(
                "../tests/fixtures/journal-minimal-evidence.json"
            ))
            .unwrap();
            document["api_base_url"] = json!(self.origin);
            document["prepared_assignments"] = json!(entry.into_iter().collect::<Vec<_>>());
            atomic_write(
                &self.directory,
                "journal.json",
                &serde_json::to_vec(&document).unwrap(),
            )
            .unwrap();
        }

        fn cache(&self, generation: u64, entries: Vec<Value>) {
            atomic_write(
                &self.directory,
                "compiled-installations.json",
                &serde_json::to_vec(
                    &json!({"version":1,"generation":generation,"installations":entries}),
                )
                .unwrap(),
            )
            .unwrap();
        }

        fn entries(&self) -> Vec<Value> {
            cache(&self.directory).unwrap()["installations"]
                .as_array()
                .unwrap()
                .clone()
        }

        fn archive(&self, entry: Value) {
            let archived = self.directory.join("archive");
            local_process::create_private_directory(&archived).unwrap();
            atomic_write(
                &archived,
                &format!("{}.json", entry["assignment"]["assignment_id"].as_str().unwrap()),
                &serde_json::to_vec(&json!({"version":1,"journal_id":entry["assignment"]["journal_id"],"entry":entry})).unwrap(),
            )
            .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    async fn handle(State(state): State<Arc<Mutex<Peer>>>, request: Request<Body>) -> Response {
        let method = request.method().to_string();
        let path = request.uri().path().to_owned();
        let bytes = to_bytes(request.into_body(), 64 * 1024).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        let mut peer = state.lock().unwrap();
        peer.requests
            .push((method.clone(), path.clone(), body.clone()));
        if method == "GET" && path == "/connector/v1/configuration" {
            let mut configuration = peer.configuration.clone();
            configuration["generation"] = json!(peer.generation);
            return Json(configuration).into_response();
        }
        if method == "POST" && path.ends_with("/retention-dispositions") {
            assert_eq!(
                path,
                format!(
                    "/connector/v1/configuration/{}/retention-dispositions",
                    peer.generation
                )
            );
            if matches!(peer.mode, Disposition::Missing) {
                return StatusCode::NOT_FOUND.into_response();
            }
            if matches!(peer.mode, Disposition::Changed) && peer.generation == 1 {
                peer.generation = 2;
                return Json(
                    json!({"generation":2,"configuration_changed":true,"dispositions":[]}),
                )
                .into_response();
            }
            let generation = peer.generation + u64::from(matches!(peer.mode, Disposition::Stale));
            let dispositions: Vec<_> = body["bindings"].as_array().unwrap().iter().map(|key| {
                let exact = key["context_id"] == "test-context" && key["provider_binding_id"] == "scalar-double-r1";
                let status = match peer.mode {
                    Disposition::Pinned => "work_or_delivery_pinned",
                    Disposition::Eligible | Disposition::Changed if exact => "eligible",
                    _ => "unmanaged",
                };
                json!({"context_id":key["context_id"],"provider_binding_id":key["provider_binding_id"],"state":status})
            }).collect();
            return Json(json!({"generation":generation,"configuration_changed":false,"dispositions":dispositions})).into_response();
        }
        if method == "PUT" && path.ends_with("/ack") {
            assert_eq!(
                path,
                format!("/connector/v1/configuration/{}/ack", peer.generation)
            );
            let publications = peer.configuration["publications"].as_array().unwrap();
            assert_eq!(
                body["validations"].as_array().unwrap().len(),
                publications.len()
            );
            for (validation, publication) in body["validations"]
                .as_array()
                .unwrap()
                .iter()
                .zip(publications)
            {
                assert_eq!(validation["publication_id"], publication["publication_id"]);
                assert_eq!(validation["ready"], true);
            }
            return Json(json!({"acknowledged":true})).into_response();
        }
        panic!("unexpected configuration request: {method} {path}");
    }

    const LAUNCHER: &str = r#"import base64,json,sys
request=json.load(sys.stdin)
assert request['version']==1
assert sys.argv[1:3]==['compute-runner',request['action']]
assert sys.argv[3]=='--directory'
assert sys.argv[4].startswith('/')
if request['action']=='protect-installations':
    references=request['payload']['references']
    assert 1<=len(references)<=128
    for reference in references:
        assert reference['release_path'].startswith('/')
        assert reference['files'] and all(path.startswith('/') for path in reference['files'])
    reply={'status':'completed'}
elif request['action']=='validate':
    assert request['payload']['descriptor']['entrypoint']
    assert request['payload']['installation_roots']==['/opt/models']
    descriptor=request['payload']['descriptor']
    entrypoint=descriptor['release_path'].rstrip('/')+'/'+descriptor['entrypoint'][0]
    reply={'status':'ready','execution_uid':str(__import__('os').getuid()+1),'file_digests':{entrypoint:base64.b64encode(b'v'*32).decode('ascii')}}
else:
    raise AssertionError(request['action'])
print(json.dumps({'version':1,'payload':reply}))
"#;

    fn binding(context: &str, provider: &str) -> Value {
        let mut descriptor: InstallationDescriptor =
            serde_json::from_value(assignment()["assignment"]["descriptor"].clone()).unwrap();
        descriptor.provider_binding_id = provider.into();
        descriptor
            .environment
            .insert("RETAINED_EVIDENCE".into(), "unchanged".into());
        let installation = compute::ComputeInstallation {
            execution_mode: Default::default(),
            provider_binding_id: provider.into(),
            context_id: context.into(),
            installation: local_process::Installation {
                provider_binding_id: provider.into(),
                installation: descriptor.installation.clone(),
                argv: vec![
                    Path::new(&descriptor.release_path)
                        .join(&descriptor.entrypoint[0])
                        .to_string_lossy()
                        .into_owned(),
                ],
                environment: descriptor.environment.clone(),
                file_digests: BTreeMap::new(),
            },
            descriptor,
            work_root: "/tmp/enumeray-retention-host-work".into(),
            connection: ConnectionConfig::Direct {
                launcher: vec!["/usr/bin/python3".into(), "-c".into(), LAUNCHER.into()],
            },
            identity_file: None,
            driver: DriverConfig::Process { scratch: None },
            slurm_script: None,
            shared_workspace: None,
        };
        serde_json::to_value(installation).unwrap()
    }

    fn assignment() -> Value {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        fixture["prepared_assignments"][0].clone()
    }

    #[test]
    fn pruning_removes_only_the_exact_authorized_unpinned_key() {
        let fixture = Fixture::new(Disposition::Eligible);
        let retained = vec![
            binding("other-context", "scalar-double-r1"),
            binding("test-context", "other-binding"),
            binding("unmanaged-context", "manual-unknown"),
        ];
        let mut entries = vec![binding("test-context", "scalar-double-r1")];
        entries.extend(retained.clone());
        fixture.cache(1, entries);
        reconcile(&fixture.client(), &fixture.directory, None).unwrap();
        assert_eq!(fixture.entries(), retained);
        let report = summary(&fixture.directory).unwrap();
        assert_eq!(report["pruned_bindings"], 1);
        assert_eq!(report["local_pins"], json!([]));
    }

    #[test]
    fn pruning_preserves_pending_unknown_and_archived_cleanup_references() {
        for pin in ["unknown", "pending_delivery", "archived"] {
            let fixture = Fixture::new(Disposition::Eligible);
            let original = fixture.entries();
            let mut entry = assignment();
            if pin == "archived" {
                fixture.archive(entry);
            } else {
                entry["terminal_result"] = Value::Null;
                entry["execution"]["receipt"] = Value::Null;
                if pin == "unknown" {
                    entry["submission_state"] = json!("unknown");
                    entry["execution"] = Value::Null;
                }
                fixture.journal(Some(entry));
            }
            reconcile(&fixture.client(), &fixture.directory, None).unwrap();
            assert_eq!(fixture.entries(), original, "{pin}");
            let report = summary(&fixture.directory).unwrap();
            assert_eq!(report["pruned_bindings"], 0, "{pin}");
            assert_eq!(report["local_pins"].as_array().unwrap().len(), 1, "{pin}");
        }
    }

    #[test]
    fn missing_unmanaged_or_server_pinned_dispositions_keep_the_cache() {
        for mode in [
            Disposition::Missing,
            Disposition::Unmanaged,
            Disposition::Pinned,
        ] {
            let fixture = Fixture::new(mode);
            let original = fixture.entries();
            reconcile(&fixture.client(), &fixture.directory, None).unwrap();
            assert_eq!(fixture.entries(), original);
            assert_eq!(summary(&fixture.directory).unwrap()["pruned_bindings"], 0);
        }
    }

    #[test]
    fn stale_dispositions_and_newer_cache_generations_never_prune() {
        let fixture = Fixture::new(Disposition::Stale);
        let original = fs::read(fixture.directory.join("compiled-installations.json")).unwrap();
        assert!(prepare_configuration(&fixture.client(), &fixture.directory, None).is_err());
        assert_eq!(
            fs::read(fixture.directory.join("compiled-installations.json")).unwrap(),
            original
        );
        assert!(
            !fixture
                .directory
                .join("configuration-retention.json")
                .exists()
        );
        assert_eq!(fixture.peer.lock().unwrap().requests.len(), 8);

        let fixture = Fixture::new(Disposition::Eligible);
        let client = fixture.client();
        let prepared = prepare_configuration(&client, &fixture.directory, None).unwrap();
        fixture.cache(2, fixture.entries());
        let original = fs::read(fixture.directory.join("compiled-installations.json")).unwrap();
        assert!(apply_configuration(&fixture.directory, prepared).is_err());
        assert_eq!(
            fs::read(fixture.directory.join("compiled-installations.json")).unwrap(),
            original
        );
    }

    #[test]
    fn changed_server_generation_refetches_before_pruning_and_acknowledgement() {
        let fixture = Fixture::new(Disposition::Changed);
        reconcile(&fixture.client(), &fixture.directory, None).unwrap();
        assert!(fixture.entries().is_empty());
        assert_eq!(cache(&fixture.directory).unwrap()["generation"], 2);
        let peer = fixture.peer.lock().unwrap();
        assert_eq!(peer.requests.len(), 5);
        assert_eq!(
            peer.requests.last().unwrap().1,
            "/connector/v1/configuration/2/ack"
        );
    }

    #[test]
    fn a_journal_change_after_inventory_blocks_pruning_until_refreshed() {
        let fixture = Fixture::new(Disposition::Eligible);
        let client = fixture.client();
        let prepared = prepare_configuration(&client, &fixture.directory, None).unwrap();
        let mut document: Value =
            local_process::read_record(&fixture.directory.join("journal.json")).unwrap();
        document["active_session_id"] = json!(Uuid::new_v4());
        atomic_write(
            &fixture.directory,
            "journal.json",
            &serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        apply_configuration(&fixture.directory, prepared).unwrap();
        assert_eq!(
            fixture.entries(),
            vec![binding("test-context", "scalar-double-r1")]
        );
        reconcile(&client, &fixture.directory, None).unwrap();
        assert!(fixture.entries().is_empty());
    }
    #[test]
    fn unmanaged_cached_metadata_retains_itself_without_blocking_protected_cache_pruning() {
        let fixture = Fixture::new(Disposition::Eligible);
        let mut original = fixture.entries();
        original
            .push(json!({"provider_binding_id":"manual-unknown","retained_evidence":"unchanged"}));
        fixture.cache(1, original.clone());
        reconcile(&fixture.client(), &fixture.directory, None).unwrap();
        assert_eq!(fixture.entries(), vec![original[1].clone()]);
        let report = summary(&fixture.directory).unwrap();
        assert_eq!(report["pruned_bindings"], 1);
        assert_eq!(report["release_protection_ready"], false);
        assert!(
            fixture
                .peer
                .lock()
                .unwrap()
                .requests
                .iter()
                .any(|(method, path, _)| method == "PUT" && path.ends_with("/ack"))
        );
    }

    #[test]
    fn a_failed_host_does_not_prevent_other_hosts_from_seeding_and_pruning() {
        let fixture = Fixture::new(Disposition::Eligible);
        let mut failed = binding("failed-context", "failed-binding");
        failed["connection"]["launcher"][2] = json!(
            "# fail before the healthy transport in sorted host order\nimport json,sys; request=json.load(sys.stdin); assert request['action']=='protect-installations'; print(json.dumps({'version':1,'payload':{'status':'pending'}}))"
        );
        fixture.cache(
            1,
            vec![failed.clone(), binding("test-context", "scalar-double-r1")],
        );
        reconcile(&fixture.client(), &fixture.directory, None).unwrap();
        assert_eq!(fixture.entries(), vec![failed]);
        let report = summary(&fixture.directory).unwrap();
        assert_eq!(report["pruned_bindings"], 1);
        assert_eq!(report["release_protection_ready"], false);
        assert_eq!(
            report["protection_blocked_bindings"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            report["protection_blocked_bindings"][0]["context_id"],
            "failed-context"
        );
        assert_eq!(
            report["protection_blocked_bindings"][0]["provider_binding_id"],
            "failed-binding"
        );
    }

    #[cfg(unix)]
    #[test]
    fn protected_local_alias_survives_cache_pruning_while_another_host_blocks_cleanup() {
        let fixture = Fixture::new(Disposition::Eligible);
        let release = fixture
            .directory
            .join("local-jobs")
            .join(Uuid::new_v4().to_string())
            .join("workspace/release");
        fs::create_dir_all(&release).unwrap();
        let alias = fixture.directory.join("installation-alias");
        std::os::unix::fs::symlink(&release, &alias).unwrap();
        let mut healthy = binding("test-context", "scalar-double-r1");
        healthy["descriptor"]["release_path"] = json!(alias);
        healthy["installation"]["argv"][0] =
            json!(alias.join(healthy["descriptor"]["entrypoint"][0].as_str().unwrap()));
        let mut failed = binding("failed-context", "failed-binding");
        failed["connection"]["launcher"][2] = json!(
            "# fail before the healthy transport in sorted host order\nimport json,sys; request=json.load(sys.stdin); print(json.dumps({'version':1,'payload':{'status':'pending'}}))"
        );
        fixture.cache(1, vec![healthy, failed.clone()]);
        reconcile(&fixture.client(), &fixture.directory, None).unwrap();
        assert_eq!(fixture.entries(), vec![failed]);
        assert_eq!(
            summary(&fixture.directory).unwrap()["release_protection_ready"],
            false
        );
        let permanent: Value =
            local_process::read_record(&fixture.directory.join("release-retention-pins.json"))
                .unwrap();
        assert!(
            permanent["references"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reference| reference["release_path"] == release.to_string_lossy().as_ref())
        );

        // Once the unrelated transport is repaired, the omitted descriptor's
        // canonical local overlap must still enter the enabled protection list.
        fixture.cache(1, vec![binding("failed-context", "failed-binding")]);
        reconcile(&fixture.client(), &fixture.directory, None).unwrap();
        assert_eq!(
            summary(&fixture.directory).unwrap()["release_protection_ready"],
            true
        );
        let protection: Value =
            local_process::read_record(&fixture.directory.join("release-protection.json")).unwrap();
        assert!(
            protection["references"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reference| reference["release_path"] == release.to_string_lossy().as_ref())
        );
        assert!(release.exists());
    }

    #[test]
    fn failed_local_protection_keeps_otherwise_eligible_cache_entries() {
        let fixture = Fixture::new(Disposition::Eligible);
        let original = fixture.entries();
        // Invalid durable overlap evidence cannot authorize dropping a known
        // descriptor, even if its compute host seed succeeds.
        atomic_write(
            &fixture.directory,
            "release-retention-pins.json",
            b"{} invalid",
        )
        .unwrap();
        reconcile(&fixture.client(), &fixture.directory, None).unwrap();
        assert_eq!(fixture.entries(), original);
        let report = summary(&fixture.directory).unwrap();
        assert_eq!(report["pruned_bindings"], 0);
        assert_eq!(report["release_protection_ready"], false);
        assert_eq!(
            report["protection_blocked_bindings"][0]["provider_binding_id"],
            "scalar-double-r1"
        );
    }

    #[test]
    fn a_configuration_is_durably_committed_before_acknowledgment_is_dispatched() {
        let fixture = Fixture::new(Disposition::Eligible);
        let client = fixture.client();
        let prepared = prepare_configuration(&client, &fixture.directory, None).unwrap();
        let committed = apply_configuration(&fixture.directory, prepared).unwrap();
        assert!(fixture.entries().is_empty());
        assert!(
            fixture
                .directory
                .join("configuration-retention.json")
                .exists()
        );
        assert!(
            fixture
                .peer
                .lock()
                .unwrap()
                .requests
                .iter()
                .all(|(_, path, _)| !path.ends_with("/ack"))
        );
        acknowledge_configuration(&client, committed).unwrap();
        assert!(
            fixture
                .peer
                .lock()
                .unwrap()
                .requests
                .last()
                .unwrap()
                .1
                .ends_with("/ack")
        );
    }

    #[test]
    fn an_active_cleanup_fence_defers_configuration_commit_without_changing_the_cache() {
        let fixture = Fixture::new(Disposition::Eligible);
        let client = fixture.client();
        let prepared = prepare_configuration(&client, &fixture.directory, None).unwrap();
        let original = fs::read(fixture.directory.join("compiled-installations.json")).unwrap();
        let cleanup = crate::cleanup::configuration_fence(&fixture.directory, true).unwrap();
        let error = apply_configuration(&fixture.directory, prepared)
            .err()
            .unwrap();
        assert_eq!(error.kind, crate::ErrorKind::Waiting);
        assert_eq!(
            fs::read(fixture.directory.join("compiled-installations.json")).unwrap(),
            original
        );
        assert!(
            !fixture
                .directory
                .join("configuration-retention.json")
                .exists()
        );
        assert!(
            fixture
                .peer
                .lock()
                .unwrap()
                .requests
                .iter()
                .all(|(_, path, _)| !path.ends_with("/ack"))
        );
        drop(cleanup);
        let prepared = prepare_configuration(&client, &fixture.directory, None).unwrap();
        let committed = apply_configuration(&fixture.directory, prepared).unwrap();
        assert!(fixture.entries().is_empty());
        acknowledge_configuration(&client, committed).unwrap();
    }

    #[test]
    fn failed_host_pin_seed_keeps_eligible_cache_while_scientific_validation_continues() {
        let fixture = Fixture::new(Disposition::Eligible);
        let mut original = binding("test-context", "scalar-double-r1");
        original["connection"]["launcher"][2] = json!(
            "import json,sys; request=json.load(sys.stdin); assert request['action']=='protect-installations'; print(json.dumps({'version':1,'payload':{'status':'pending'}}))"
        );
        fixture.cache(1, vec![original.clone()]);
        let fresh = binding("desired-context", "fresh-binding");
        fixture.peer.lock().unwrap().configuration = json!({
            "run_payload_namespace_reserved":true,
            "contexts":[{"id":"desired-context","definition":{"driver":fresh["driver"],"connection":fresh["connection"],"installation_roots":["/opt/models"],"work_root":fresh["work_root"],"max_outstanding_assignments":1}}],
            "publications":[{"publication_id":"fresh-publication","context_id":"desired-context","generation":1,"descriptor":fresh["descriptor"],"requirements":assignment()["assignment"]["requirements"]}],
        });
        reconcile(&fixture.client(), &fixture.directory, None).unwrap();
        let entries = fixture.entries();
        assert!(entries.contains(&original));
        assert!(
            entries
                .iter()
                .any(|entry| entry["provider_binding_id"] == "fresh-binding")
        );
        let report = summary(&fixture.directory).unwrap();
        assert_eq!(report["pruned_bindings"], 0);
        assert_eq!(report["release_protection_ready"], false);
        let peer = fixture.peer.lock().unwrap();
        let (_, _, ack) = peer
            .requests
            .iter()
            .find(|(method, path, _)| method == "PUT" && path.ends_with("/ack"))
            .unwrap();
        assert_eq!(ack["validations"][0]["publication_id"], "fresh-publication");
        assert_eq!(ack["validations"][0]["ready"], true);
    }
}
