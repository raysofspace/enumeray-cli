//! Run command workflows.

use super::{get, paged_get, query_get};
use crate::{args::Run, transfers};
use enumeray_client::resource_path;
use enumeray_client::{EnumerayClient, Error};
use reqwest::Method;
use serde_json::Value;

pub(crate) fn run(client: &EnumerayClient, command: &Run) -> Result<Value, Error> {
    match command {
        Run::Collections { id } => get(client, &["runs", id, "collections"]),
        Run::List { page, filters } => {
            let mut query = page.query();
            query.extend(filters.query());
            query_get(client, &["runs"], query)
        }
        Run::Get { id } => get(client, &["runs", id]),
        Run::Wait { .. } => unreachable!("observation dispatch"),
        Run::LogArchive { id } => get(client, &["runs", id, "log-archive"]),
        Run::Logs { id, page, .. } => paged_get(client, &["runs", id, "logs"], page),
        Run::Events { id, page } => paged_get(client, &["runs", id, "events"], page),
        Run::Artifacts { id, page } => paged_get(client, &["runs", id, "artifacts"], page),
        Run::Provenance { id, output } => {
            if let Some(output) = output {
                transfers::download(
                    client,
                    &resource_path(&["runs", id, "provenance", "content"])?,
                    output,
                    None,
                )
            } else {
                get(client, &["runs", id, "provenance"])
            }
        }
        Run::Cancel { id } => client.request::<Value, Value>(
            Method::PUT,
            &resource_path(&["runs", id, "cancellation"])?,
            None,
            None,
        ),
    }
}
