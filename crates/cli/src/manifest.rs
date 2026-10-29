//! Local manifest parsing and publication validation, independent of credentials.

use crate::config;
use enumeray_client::Error;
use enumeray_contracts::management::PublicationCreate;
use serde::{Deserialize, de::DeserializeOwned};
use std::{io::Read, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest<T> {
    manifest_version: u32,
    kind: String,
    spec: T,
}

pub(crate) fn manifest<T: DeserializeOwned>(file: &Path, kind: &str) -> Result<T, Error> {
    let manifest: Manifest<T> = config::read_json(file)?;
    if manifest.manifest_version != 1 || manifest.kind != kind {
        return Err(Error::Invalid(format!(
            "Expected manifest_version 1 and kind {kind}."
        )));
    }
    Ok(manifest.spec)
}

/// Author input is separate from generated release snapshots and API requests.
pub(crate) fn publication_source(
    file: &Path,
) -> Result<
    (
        serde_json::Value,
        enumeray_contracts::models::ParsedPublication,
    ),
    Error,
> {
    if !std::fs::metadata(file)?.is_file() {
        return Err(Error::Invalid(
            "Publication source must be a regular file.".into(),
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(file)?
        .take(enumeray_contracts::models::MAX_IMPORT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let source = enumeray_contracts::models::parse_publication(&bytes)
        .map_err(|error| Error::Invalid(error.to_string()))?;
    Ok((serde_json::from_slice(&bytes)?, source))
}

pub(crate) fn validate_publication(candidate: &PublicationCreate) -> Result<(), Error> {
    enumeray_contracts::models::validate_publication(candidate)
        .map_err(|error| Error::Invalid(error.to_string()))
}
