//! Private directory ownership, exclusive writers and immutable operation evidence.
use crate::{
    config,
    transfers::{self, UploadOperation},
};
use enumeray_client::{Error, PreparedRun, RunPurpose, SubmissionReceipt};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};
use uuid::Uuid;

use super::recovery::Recovery;

const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;

pub(super) enum AnalysisState {
    Local(Saved),
    Recovery(Recovery),
}

impl AnalysisState {
    pub fn local(&self) -> Option<&Saved> {
        match self {
            Self::Local(saved) => Some(saved),
            Self::Recovery(_) => None,
        }
    }
    pub fn api_url(&self) -> &str {
        match self {
            Self::Local(saved) => &saved.api_url,
            Self::Recovery(saved) => saved.api_url(),
        }
    }
    pub fn user_id(&self) -> Uuid {
        match self {
            Self::Local(saved) => saved.user_id,
            Self::Recovery(saved) => saved.user_id(),
        }
    }
    pub fn model_id(&self) -> &str {
        match self {
            Self::Local(saved) => &saved.model_id,
            Self::Recovery(saved) => saved.model_id(),
        }
    }
    pub fn publication_id(&self) -> &str {
        match self {
            Self::Local(saved) => &saved.publication_id,
            Self::Recovery(saved) => saved.publication_id(),
        }
    }
    pub fn purpose(&self) -> RunPurpose {
        match self {
            Self::Local(saved) => saved.purpose,
            Self::Recovery(saved) => saved.purpose(),
        }
    }
    pub fn publication_delegation_id(&self) -> Option<Uuid> {
        match self {
            Self::Local(saved) => saved.publication_delegation_id,
            Self::Recovery(saved) => saved.publication_delegation_id(),
        }
    }
    pub fn creator_id(&self) -> Option<Uuid> {
        match self {
            Self::Local(saved) => Some(saved.user_id),
            Self::Recovery(saved) => saved.creator_id(),
        }
    }
    pub fn run_id(&self) -> Option<String> {
        match self {
            Self::Local(saved) => saved
                .receipt
                .as_ref()
                .map(|receipt| receipt.run_id.to_string()),
            Self::Recovery(saved) => saved.run_id(),
        }
    }
    pub fn requested_inputs(&self) -> Result<Value, Error> {
        match self {
            Self::Local(saved) => super::effective_inputs(saved),
            Self::Recovery(saved) => saved.requested_inputs(),
        }
    }
    pub fn effective_inputs(&self) -> Result<Value, Error> {
        match self {
            Self::Local(saved) => super::effective_inputs(saved),
            Self::Recovery(saved) => saved.effective_inputs(),
        }
    }
    pub fn definition(&self) -> Result<enumeray_contracts::models::ScientificDefinition, Error> {
        match self {
            Self::Local(saved) => Ok(serde_json::from_value(
                saved.publication["definition"].clone(),
            )?),
            Self::Recovery(saved) => saved.definition(),
        }
    }
    pub fn definition_value(&self) -> Result<Value, Error> {
        match self {
            Self::Local(saved) => Ok(saved.publication["definition"].clone()),
            Self::Recovery(saved) => saved.definition_value(),
        }
    }
    pub fn identity(&self) -> Value {
        match self {
            Self::Local(saved) => {
                serde_json::json!({"model_id":saved.model_id,"publication_id":saved.publication_id,"purpose":saved.purpose,
                "grant_id":saved.publication["grant_id"],"publication_delegation_id":saved.publication_delegation_id,"context_id":saved.publication["context_id"],
                "installation_status":saved.publication["status"],"installation":saved.publication["descriptor"]})
            }
            Self::Recovery(saved) => saved.identity(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub id: Uuid,
    pub operation: UploadOperation,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CollectionBinding {
    pub id: Uuid,
    pub operation: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Saved {
    pub schema_version: u32,
    pub api_url: String,
    pub user_id: Uuid,
    pub model_id: String,
    pub publication_id: String,
    #[serde(default)]
    pub purpose: RunPurpose,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_delegation_id: Option<Uuid>,
    pub publication: Value,
    pub bindings: BTreeMap<String, Binding>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub collection_bindings: BTreeMap<String, CollectionBinding>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub collection_uploads: BTreeMap<String, Value>,
    pub run_key: String,
    pub frozen_inputs: Option<Value>,
    pub input_artifacts: BTreeMap<String, Value>,
    pub uploads: BTreeMap<String, Value>,
    pub prepared: Option<PreparedRun>,
    pub submission_started: bool,
    pub receipt: Option<SubmissionReceipt>,
}

pub(super) struct Directory {
    pub root: PathBuf,
    writer: Option<File>,
}

pub(super) fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

pub(crate) fn private_dir(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid(
            "Analysis directories must be real directories, not symlinks.",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid(
                "Analysis directory permits group or other access; set permissions to 0700.",
            ));
        }
    }
    Ok(())
}

impl Directory {
    /// Only recovery journals authorize resuming an existing destination. A
    /// directory without one remains untouched, even when it happens to be empty.
    pub fn recovery(root: &Path, initial: &Recovery) -> Result<Self, Error> {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        let created = match builder.create(root) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error.into()),
        };
        private_dir(root)?;
        let mut directory = Self {
            root: root.canonicalize()?,
            writer: None,
        };
        if !created && !directory.exists("recovery.json")? {
            return Err(invalid(
                "Recovery destination already exists without a recovery journal; preserve it and choose a new directory.",
            ));
        }
        directory.claim_writer()?;
        if created {
            let bytes = serde_json::to_vec(initial)?;
            if bytes.len() as u64 > MAX_STATE_BYTES {
                return Err(invalid("Analysis recovery journal exceeds 8 MiB."));
            }
            config::write_private_bytes(&directory.root.join("recovery.json"), &bytes, false)?;
            File::open(directory.root.parent().unwrap_or(Path::new(".")))?.sync_all()?;
        } else {
            let retained: Recovery = directory.read("recovery.json")?;
            retained.validate()?;
            if !retained.same_source(initial)? {
                return Err(invalid(
                    "Recovery source conflicts with this directory; preserve both and choose a new destination.",
                ));
            }
        }
        for relative in ["state", "state/uploads", "results", "results/files"] {
            match builder.create(directory.root.join(relative)) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
            private_dir(&directory.root.join(relative))?;
            File::open(directory.root.join(relative))?.sync_all()?;
        }
        File::open(&directory.root)?.sync_all()?;
        Ok(directory)
    }

    pub fn write_inputs(&self, inputs: &Value) -> Result<(), Error> {
        // Formatting must not make a valid compact input exceed the raw JSON reader's limit.
        let mut bytes = serde_json::to_vec_pretty(inputs)?;
        bytes.push(b'\n');
        if bytes.len() > enumeray_contracts::models::MAX_INPUT_BYTES {
            bytes = serde_json::to_vec(inputs)?;
        }
        config::write_private_bytes(&self.root.join("inputs.json"), &bytes, false)
    }
    pub fn inputs(&self) -> Result<Value, Error> {
        let path = self.root.join("inputs.json");
        config::require_private(&path)?;
        super::read_inputs(&path)
    }
    pub fn create(root: &Path) -> Result<Self, Error> {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(root).map_err(|error| if error.kind() == std::io::ErrorKind::AlreadyExists {
            invalid("Analysis destination already exists; use its saved operation or choose a new directory.")
        } else { error.into() })?;
        for relative in ["state", "state/uploads", "results", "results/files"] {
            builder.create(root.join(relative))?;
        }
        for relative in ["state/uploads", "state", "results/files", "results", ""] {
            File::open(root.join(relative))?.sync_all()?;
        }
        File::open(
            root.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        let mut directory = Self::open(root)?;
        directory.claim_writer()?;
        Ok(directory)
    }

    pub fn open(root: &Path) -> Result<Self, Error> {
        private_dir(root)?;
        let root = root.canonicalize()?;
        for relative in ["state", "state/uploads", "results", "results/files"] {
            private_dir(&root.join(relative))?;
        }
        Ok(Self { root, writer: None })
    }

    /// Submit and collection own the writer for their full lifetime, including transfers.
    /// Readers and independent observation/control checkpoints never take this lock.
    pub fn claim_writer(&mut self) -> Result<(), Error> {
        let lock = self.open_lock("analysis.lock")?;
        lock.try_lock().map_err(|_| invalid("Analysis directory already has a submit/collect writer or cannot be locked; resume after that writer exits. Status, logs, wait and cancellation remain available."))?;
        self.writer = Some(lock);
        Ok(())
    }

    fn open_lock(&self, relative: &str) -> Result<File, Error> {
        let path = self.root.join(relative);
        let mut options = OpenOptions::new();
        options.write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = match options.create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                config::require_private(&path)?;
                options.create_new(false).open(&path)?
            }
            Err(error) => return Err(error.into()),
        };
        config::require_private(&path)?;
        Ok(lock)
    }

    /// Only local bounded reads and atomic publications belong inside this lock.
    /// Lock order is writer/control ownership, then state; never the reverse.
    pub fn state_lock(&self) -> Result<File, Error> {
        let lock = self.open_lock("state.lock")?;
        lock.lock()?;
        Ok(lock)
    }

    pub fn read<T: DeserializeOwned>(&self, relative: &str) -> Result<T, Error> {
        let path = self.root.join(relative);
        config::require_private(&path)?;
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(invalid("Analysis evidence exceeds 8 MiB."));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn exists(&self, relative: &str) -> Result<bool, Error> {
        match fs::symlink_metadata(self.root.join(relative)) {
            Ok(_) => {
                config::require_private(&self.root.join(relative))?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn write<T: Serialize>(&self, relative: &str, value: &T) -> Result<(), Error> {
        let _snapshot = self.state_lock()?;
        self.write_locked(relative, value)
    }

    pub(super) fn write_locked<T: Serialize>(
        &self,
        relative: &str,
        value: &T,
    ) -> Result<(), Error> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(invalid("Analysis evidence exceeds 8 MiB."));
        }
        let replace = self.exists(relative)?;
        config::write_private_bytes(&self.root.join(relative), &bytes, replace)
    }

    /// An interrupted previous publication may be adopted only when it is identical.
    pub fn evidence<T: Serialize>(&self, relative: &str, value: &T) -> Result<(), Error> {
        let _snapshot = self.state_lock()?;
        self.check_evidence(relative, value, true)
    }

    pub(super) fn check_evidence<T: Serialize>(
        &self,
        relative: &str,
        value: &T,
        repair: bool,
    ) -> Result<(), Error> {
        if self.exists(relative)? {
            if self.read::<Value>(relative)? != serde_json::to_value(value)? {
                return Err(invalid(format!(
                    "Saved analysis evidence conflicts at {relative}; preserve it and inspect this directory."
                )));
            }
            Ok(())
        } else if repair {
            self.write_locked(relative, value)
        } else {
            // The journal contains the identical evidence. Only a writer may repair it.
            Ok(())
        }
    }

    /// Caller holds state_lock; account discovery must finish before taking it.
    /// Read-only callers adopt published receipts in memory without repairing the journal.
    pub fn load(&self, api_url: &str, user_id: Uuid) -> Result<AnalysisState, Error> {
        if self.exists("recovery.json")? {
            return super::recovery::load(self, api_url, user_id, self.writer.is_some())
                .map(AnalysisState::Recovery);
        }
        self.load_local(api_url, user_id).map(AnalysisState::Local)
    }

    fn load_local(&self, api_url: &str, user_id: Uuid) -> Result<Saved, Error> {
        let repair = self.writer.is_some();
        let mut saved: Saved = self.read("analysis.json")?;
        if saved.schema_version != 1 || saved.api_url != api_url || saved.user_id != user_id {
            return Err(invalid(
                "Analysis belongs to another API/account or uses an unsupported format.",
            ));
        }
        if self.read::<Value>("state/publication.json")? != saved.publication {
            return Err(invalid("Saved publication evidence changed."));
        }
        if saved.purpose.is_standard() {
            if saved.publication_delegation_id.is_some()
                || saved.publication.get("release_authority").is_some()
            {
                return Err(invalid(
                    "Standard analysis has candidate-trial authority evidence.",
                ));
            }
        } else {
            let selected = crate::candidate::validate(
                saved.publication.clone(),
                &saved.model_id,
                &saved.publication_id,
            )?;
            crate::candidate::validate_actor(&selected.publication, saved.user_id)?;
            if selected.publication != saved.publication
                || selected.publication_delegation_id != saved.publication_delegation_id
            {
                return Err(invalid("Saved candidate-trial authority is inconsistent."));
            }
        }
        if saved.publication["publication_id"] != saved.publication_id
            || saved.publication["definition"].is_null()
            || saved.run_key.parse::<Uuid>().is_err()
            || (saved.submission_started != saved.prepared.is_some())
            || (saved.frozen_inputs.is_none()
                && (!saved.uploads.is_empty() || saved.prepared.is_some()))
            || (saved.receipt.is_some() && !saved.submission_started)
            || saved
                .uploads
                .keys()
                .any(|key| !saved.bindings.contains_key(key))
        {
            return Err(invalid("Analysis operation state is inconsistent."));
        }
        let definition: enumeray_contracts::models::ScientificDefinition =
            serde_json::from_value(saved.publication["definition"].clone())?;
        let declarations = enumeray_contracts::collections::declarations(&definition, false)
            .map_err(|e| invalid(e.to_string()))?;
        for (name, binding) in &saved.collection_bindings {
            let suffix = binding
                .operation
                .strip_prefix("state/collections/")
                .ok_or_else(|| invalid("Unsafe collection operation path."))?;
            if !declarations.contains_key(name)
                || binding.id.is_nil()
                || Uuid::parse_str(suffix).is_err()
                || saved.bindings.contains_key(name)
            {
                return Err(invalid("Saved collection binding is inconsistent."));
            }
        }
        if saved
            .collection_uploads
            .keys()
            .any(|k| !saved.collection_bindings.contains_key(k))
            || (saved.frozen_inputs.is_none() && !saved.collection_uploads.is_empty())
        {
            return Err(invalid("Saved collection completion is inconsistent."));
        }
        for (name, binding) in &saved.bindings {
            let operation = &binding.operation;
            if operation.api_url != saved.api_url
                || operation.user_id != saved.user_id
                || operation.schema_version != 1
                || operation.request.logical_name != *name
                || operation.reservation.is_some()
                || operation.version_id.is_some()
                || operation.multipart.is_some()
                || !operation.source.is_absolute()
                || binding.id.is_nil()
                || operation.idempotency_key.parse::<Uuid>().is_err()
            {
                return Err(invalid("Analysis upload identity is inconsistent."));
            }
            let path = upload_path(binding);
            let current: UploadOperation = self.read(&path)?;
            let mut identity = serde_json::to_value(current)?;
            identity["reservation"] = Value::Null;
            identity["version_id"] = Value::Null;
            // Multipart progress advances independently through atomic transfer checkpoints.
            identity
                .as_object_mut()
                .expect("serialized operation")
                .remove("multipart");
            if identity != serde_json::to_value(operation)? {
                return Err(invalid("Saved upload identity changed."));
            }
            if let Some(receipt) = saved.uploads.get(name) {
                self.check_evidence(
                    &format!("state/uploads/{}.receipt.json", binding.id),
                    receipt,
                    repair,
                )?;
            }
        }
        if let Some(inputs) = &saved.frozen_inputs {
            if self.inputs()? != *inputs {
                return Err(invalid(
                    "Frozen analysis inputs changed; restore them or initialize a new directory.",
                ));
            }
            self.check_evidence("state/frozen-inputs.json", inputs, repair)?;
        }
        if let Some(prepared) = &saved.prepared {
            if prepared.schema_version != 1
                || prepared.api_url != saved.api_url
                || prepared.user_id != saved.user_id
                || prepared.idempotency_key != saved.run_key
                || prepared.request.model_id != saved.model_id
                || prepared.request.purpose != saved.purpose
                || prepared.request.publication_delegation_id != saved.publication_delegation_id
                || prepared.request.expected_publication_id != saved.publication_id
                || prepared.request.inputs != super::effective_inputs(&saved)?
            {
                return Err(invalid("Prepared analysis identity or inputs changed."));
            }
            self.check_evidence("state/prepared-run.json", prepared, repair)?;
        }
        // Receipt publication precedes journal advancement. Recover that boundary without POST.
        if self.exists("state/receipt.json")? {
            let receipt: SubmissionReceipt = self.read("state/receipt.json")?;
            validate_receipt(&saved, &receipt)?;
            if let Some(retained) = &saved.receipt {
                if serde_json::to_value(retained)? != serde_json::to_value(&receipt)? {
                    return Err(invalid("Acceptance receipt changed."));
                }
            } else {
                saved.receipt = Some(receipt);
                if repair {
                    self.write_locked("analysis.json", &saved)?;
                }
            }
        } else if saved.receipt.is_some() {
            return Err(invalid("Saved acceptance receipt is missing."));
        }
        Ok(saved)
    }

    pub fn save(&self, saved: &Saved) -> Result<(), Error> {
        self.write("analysis.json", saved)
    }

    pub fn save_recovery(&self, saved: &Recovery) -> Result<(), Error> {
        self.write("analysis.json", saved)
    }
}

pub(super) fn upload_path(binding: &Binding) -> String {
    format!("state/uploads/{}.json", binding.id)
}

pub(super) fn validate_receipt(saved: &Saved, receipt: &SubmissionReceipt) -> Result<(), Error> {
    if !saved.submission_started
        || saved.prepared.is_none()
        || receipt.run_id.is_nil()
        || receipt.model_id != saved.model_id
        || receipt.purpose != saved.purpose
        || receipt.publication_delegation_id != saved.publication_delegation_id
        || receipt.publication_id != saved.publication_id
        || receipt.state != "queued"
        || receipt.created_at.is_empty()
    {
        return Err(invalid(
            "API returned an inconsistent analysis acceptance receipt; retain this operation and retry.",
        ));
    }
    Ok(())
}

pub(super) fn verify_source(operation: &UploadOperation) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(&operation.source)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid(
            "Analysis source must remain a regular file, not a symlink.",
        ));
    }
    let (size, digest) = transfers::checksum(&mut File::open(&operation.source)?)?;
    if size != operation.request.expected_size_bytes || digest != operation.request.checksum.value {
        return Err(invalid(
            "Source file changed since analysis initialization; initialize a new directory for changed inputs.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_do_not_own_the_transfer_writer_and_evidence_remains_immutable() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("analysis");
        let writer = Directory::create(&path).unwrap();
        let mut reader = Directory::open(&path).unwrap();
        assert!(reader.claim_writer().is_err());
        let evidence = serde_json::json!({"identity": "retained"});
        writer.evidence("state/test.json", &evidence).unwrap();
        {
            let _snapshot = reader.state_lock().unwrap();
            assert_eq!(reader.read::<Value>("state/test.json").unwrap(), evidence);
            assert!(
                reader
                    .check_evidence("state/missing.json", &evidence, false)
                    .is_ok()
            );
            assert!(!reader.exists("state/missing.json").unwrap());
            assert!(
                reader
                    .check_evidence("state/test.json", &Value::Null, false)
                    .is_err()
            );
        }
        assert!(writer.evidence("state/test.json", &Value::Null).is_err());
        drop(writer);
        reader.claim_writer().unwrap();
        reader.evidence("state/test.json", &evidence).unwrap();
    }

    #[test]
    fn formatting_does_not_break_large_valid_scientific_inputs() {
        let root = tempfile::tempdir().unwrap();
        let directory = Directory::create(&root.path().join("analysis")).unwrap();
        let inputs = serde_json::json!({"values": vec![0; 20_000]});
        assert!(
            serde_json::to_vec_pretty(&inputs).unwrap().len()
                > enumeray_contracts::models::MAX_INPUT_BYTES
        );
        directory.write_inputs(&inputs).unwrap();
        assert_eq!(directory.inputs().unwrap(), inputs);
    }
}
