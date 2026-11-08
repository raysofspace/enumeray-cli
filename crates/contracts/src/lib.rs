//! Shared wire contracts and pure validation for the API and connector.

/// API origin of the hosted Enumeray service.
pub const DEFAULT_API_ORIGIN: &str = "https://api.enumeray.com";

pub mod artifact_names;
pub mod assignments;
pub mod automation;
pub mod collections;
pub mod compatibility;
pub mod execution;
pub mod logs;
pub mod management;
pub mod media;
pub mod models;
pub mod numeric;
pub mod runs;
pub mod setup;
pub mod transfers;

pub mod installation;
