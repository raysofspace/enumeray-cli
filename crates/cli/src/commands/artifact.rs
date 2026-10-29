//! Artifact command workflows.

use super::get;
use crate::{args::Artifact, config, transfers};
use enumeray_client::resource_path;
use enumeray_client::{EnumerayClient, Error};
use serde_json::{Value, json};

pub(crate) fn artifact(client: &EnumerayClient, command: &Artifact) -> Result<Value, Error> {
    match command {
        Artifact::Get { id } => get(client, &["artifacts", id]),
        Artifact::Download { id, output } => (|| {
            let metadata = get(client, &["artifacts", id])?;
            let size = metadata
                .pointer("/content/size_bytes")
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::Invalid("Artifact has no verified content.".into()))?;
            let checksum = metadata
                .pointer("/content/checksum/value")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Invalid("Artifact has no verified checksum.".into()))?;
            let version = metadata
                .pointer("/content/version_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Invalid("Artifact has no verified version.".into()))?;
            transfers::download(
                client,
                &resource_path(&["artifacts", id, "content"])?,
                output,
                Some((size, checksum.to_owned(), version.to_owned())),
            )
        })()
        .map_err(transfers::download_authorization_hint),
        Artifact::Upload {
            file,
            name,
            media_type,
            metadata,
            state,
        } => transfers::prepare(
            client,
            file,
            name.clone(),
            media_type.clone(),
            metadata
                .as_ref()
                .map(|path| config::read_inputs(path))
                .transpose()?
                .unwrap_or_else(|| json!({})),
            state,
        ),
        Artifact::Resume { state } => transfers::resume(client, state),
    }
}
