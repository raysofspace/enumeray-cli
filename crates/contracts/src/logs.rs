//! Raw source positions and infrastructure-owned diagnostic retention.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CHUNK_BYTES: usize = 2048;
/// Bounded transport read-ahead; delivery requests still consume CHUNK_BYTES.
pub const READ_WINDOW_BYTES: usize = 16 * 1024;
pub const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
pub const ARCHIVE_RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogPolicy {
    pub segment_bytes: u64,
    pub retained_segments: u32,
    pub archive_enabled: bool,
    pub archive_source_bytes: u64,
    pub archive_compressed_bytes: u64,
    pub archive_publication_seconds: u64,
}

impl Default for LogPolicy {
    fn default() -> Self {
        Self {
            segment_bytes: 64 * 1024,
            retained_segments: 16,
            archive_enabled: false,
            archive_source_bytes: 16 * 1024 * 1024,
            archive_compressed_bytes: 50 * 1024 * 1024,
            archive_publication_seconds: 24 * 60 * 60,
        }
    }
}

impl LogPolicy {
    pub fn live_bytes(&self) -> u64 {
        self.segment_bytes * u64::from(self.retained_segments)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !(4096..=1024 * 1024).contains(&self.segment_bytes)
            || !(2..=128).contains(&self.retained_segments)
            || self.live_bytes() > 16 * 1024 * 1024
            || !(self.live_bytes()..=128 * 1024 * 1024).contains(&self.archive_source_bytes)
            || self.archive_compressed_bytes > MAX_ARCHIVE_BYTES
            // Three source budgets plus bounded tar/manifest/deflate framing.
            || self.archive_compressed_bytes < self.archive_source_bytes * 3 + 2 * 1024 * 1024
            || !(1..=365 * 24 * 60 * 60).contains(&self.archive_publication_seconds)
        {
            return Err("invalid log policy or insufficient archive framing budget");
        }
        Ok(())
    }
}

/// A bounded model source identity. Its private representation cannot contain paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NamedLogSource([u8; 70]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogSource {
    Stdout,
    Stderr,
    Connector,
    SlurmBatch,
    Model(NamedLogSource),
}
impl LogSource {
    pub fn model(name: &str) -> Option<Self> {
        if !crate::models::validation::valid_model_id(name) {
            return None;
        }
        let mut bytes = [0; 70];
        bytes[..6].copy_from_slice(b"model.");
        bytes[6..6 + name.len()].copy_from_slice(name.as_bytes());
        Some(Self::Model(NamedLogSource(bytes)))
    }
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Connector => "connector",
            Self::SlurmBatch => "slurm_batch",
            Self::Model(name) => {
                std::str::from_utf8(&name.0[..name.0.iter().position(|b| *b == 0).unwrap_or(70)])
                    .expect("validated ASCII source")
            }
        }
    }
}
impl Serialize for LogSource {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for LogSource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "stdout" => Ok(Self::Stdout),
            "stderr" => Ok(Self::Stderr),
            "connector" => Ok(Self::Connector),
            "slurm_batch" => Ok(Self::SlurmBatch),
            _ => value
                .strip_prefix("model.")
                .and_then(Self::model)
                .ok_or_else(|| serde::de::Error::custom("invalid log source")),
        }
    }
}
#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for LogSource {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::schema::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .pattern(Some(
                "^(stdout|stderr|connector|slurm_batch|model\\.[a-z][a-z0-9_-]{0,63})$",
            ))
            .into()
    }
}
#[cfg(feature = "openapi")]
impl utoipa::ToSchema for LogSource {}

pub const MAX_MODEL_LOG_FILES: usize = 8;
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogFileDeclaration {
    #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 1024))]
    pub path: String,
}

pub fn valid_log_path(path: &str) -> bool {
    let first = path.split('/').next().unwrap_or("");
    !path.is_empty()
        && path.len() <= 1024
        && !path.chars().any(char::is_control)
        && !path.contains(['\\', '*', '?', '[', ']'])
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
        && !first.starts_with('.')
        && !matches!(
            first,
            "request.json" | "result.json" | "stdout.log" | "stderr.log" | "inputs" | "outputs"
        )
}

pub fn validate_log_files(
    files: &BTreeMap<String, LogFileDeclaration>,
) -> Result<(), &'static str> {
    let mut paths = std::collections::BTreeSet::new();
    if files.len() > MAX_MODEL_LOG_FILES
        || files.iter().any(|(name, file)| {
            LogSource::model(name).is_none()
                || !valid_log_path(&file.path)
                || !paths.insert(&file.path)
        })
    {
        return Err("invalid model log declarations");
    }
    Ok(())
}

pub fn deserialize_log_files<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, LogFileDeclaration>, D::Error> {
    struct Files;
    impl<'de> serde::de::Visitor<'de> for Files {
        type Value = BTreeMap<String, LogFileDeclaration>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded unique log declarations")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut files = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, LogFileDeclaration>()? {
                if files.len() == MAX_MODEL_LOG_FILES || files.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate or excessive log declarations",
                    ));
                }
            }
            validate_log_files(&files).map_err(serde::de::Error::custom)?;
            Ok(files)
        }
    }
    deserializer.deserialize_map(Files)
}

/// One authoritative set for capture, settlement, archive validation and cleanup.
pub fn expected_sources(
    definition: &crate::models::ScientificDefinition,
    slurm: bool,
) -> Vec<LogSource> {
    let mut sources = vec![LogSource::Stdout, LogSource::Stderr];
    if slurm {
        sources.push(LogSource::SlurmBatch);
    }
    sources.extend(
        definition
            .log_files
            .keys()
            .filter_map(|name| LogSource::model(name)),
    );
    sources
}

impl LogPolicy {
    /// Internal allowance; the infrastructure policy is validated before splitting.
    /// Small effective segments are intentional when a minimal policy is shared.
    pub fn for_sources(&self, count: usize, slurm: bool) -> Self {
        let base = if slurm { 3 } else { 2 };
        let mut policy = self.clone();
        policy.segment_bytes = self.segment_bytes * base / count as u64;
        policy.archive_source_bytes = self.archive_source_bytes * base / count as u64;
        policy
    }
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogGapReason {
    CaptureTruncated,
    SourceUnavailable,
    RetentionDiscarded,
    SourceReset,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogGap {
    pub reason: LogGapReason,
    /// Null only for loss whose size cannot be established, in a new epoch.
    pub bytes: Option<i64>,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogBatchRequest {
    pub attempt_id: Uuid,
    /// Start of text, after any omitted bytes, in this epoch.
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub offset: i64,
    /// Source bytes consumed by text, independent of replacement UTF-8 length.
    #[cfg_attr(feature = "openapi", schema(minimum = 0, maximum = 8192))]
    pub byte_length: i64,
    #[cfg_attr(feature = "openapi", schema(max_length = 8192))]
    pub text: String,
    #[serde(default, deserialize_with = "gap_object")]
    pub gap: Option<LogGap>,
}
fn gap_object<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<LogGap>, D::Error> {
    let raw = Option::<Box<serde_json::value::RawValue>>::deserialize(deserializer)?;
    raw.map(|raw| {
        if !raw.get().starts_with('{') {
            return Err(serde::de::Error::custom("expected gap object"));
        }
        serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)
    })
    .transpose()
}
impl LogBatchRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.attempt_id.is_nil()
            || self.offset < 0
            || !(0..=8192).contains(&self.byte_length)
            || self.offset.checked_add(self.byte_length).is_none()
            || self.text.len() > 8192
            || self.text.contains('\0')
            || self.text.is_empty() != (self.byte_length == 0)
            || self.text.is_empty() && self.gap.is_none()
            || self.gap.as_ref().is_some_and(|gap| {
                gap.bytes.is_some_and(|n| n < 1)
                    || gap.bytes.is_none()
                        && !matches!(
                            gap.reason,
                            LogGapReason::SourceReset | LogGapReason::SourceUnavailable
                        )
            })
        {
            return Err("invalid log batch");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadLogsRequest {
    pub source: LogSource,
    pub epoch: i64,
    pub offset: u64,
    pub complete: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadLogsResponse {
    pub epoch: i64,
    pub earliest: u64,
    pub end: u64,
    pub offset: u64,
    pub data: String,
    pub closed: bool,
    pub unavailable: bool,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveRange {
    pub epoch: i64,
    pub offset: u64,
    pub byte_length: u64,
    pub member: String,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveSource {
    pub ranges: Vec<ArchiveRange>,
    pub total_bytes: Option<u64>,
    pub omitted_bytes: Option<u64>,
    pub unknown_loss: bool,
    pub complete: bool,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogArchiveManifest {
    pub version: u32,
    pub assignment_id: Uuid,
    pub attempt_id: Uuid,
    pub launch_intent_id: Uuid,
    pub sources: BTreeMap<LogSource, ArchiveSource>,
    pub complete: bool,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveState {
    Disabled,
    Capturing,
    Pending,
    Ready,
    Unavailable,
    Expired,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogArchiveIntent {
    pub attempt_id: Uuid,
    pub policy: LogPolicy,
    pub finalized_at: Option<String>,
    pub diagnostic: Option<String>,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogArchiveStatus {
    pub state: ArchiveState,
    pub artifact_id: Option<Uuid>,
    pub manifest: Option<LogArchiveManifest>,
    pub expires_at: Option<String>,
    pub diagnostic: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_log_declarations_are_bounded_unique_and_backward_compatible() {
        let mut model: serde_json::Value =
            serde_json::from_str(include_str!("../../../examples/scalar-double.json")).unwrap();
        let old = serde_json::to_value(
            crate::models::parse_publication(include_bytes!(
                "../../../examples/scalar-double.json"
            ))
            .unwrap()
            .definition,
        )
        .unwrap();
        let definition: crate::models::ScientificDefinition =
            serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(definition).unwrap(), old);
        for path in [
            "",
            "/tmp/secret",
            "../secret",
            "a/../b",
            "a/./b",
            "a//b",
            "a/",
            "a\\b",
            "*.log",
            "a?[0]",
            ".enumeray/tmp/a",
            "request.json",
            "inputs/secret",
            "outputs/a",
            "result.json",
            "a\nlog",
        ] {
            model["log_files"] = serde_json::json!({"solver":{"path":path}});
            assert!(
                crate::models::parse_publication(&serde_json::to_vec(&model).unwrap()).is_err(),
                "{path:?}"
            );
        }
        model["log_files"] = serde_json::json!({"solver":{"path":"logs/solver.log"},"convergence":{"path":"logs/c.log"}});
        let parsed =
            crate::models::parse_publication(&serde_json::to_vec(&model).unwrap()).unwrap();
        assert_eq!(expected_sources(&parsed.definition, true).len(), 5);
        model["log_files"]["convergence"]["path"] = serde_json::json!("logs/solver.log");
        assert!(crate::models::parse_publication(&serde_json::to_vec(&model).unwrap()).is_err());
        assert!(
            serde_json::from_str::<crate::models::ScientificDefinition>(
                &serde_json::to_string(&old).unwrap().replacen(
                    "{",
                    r#"{"log_files":{"a":{"path":"a.log"},"a":{"path":"b.log"}},"#,
                    1
                )
            )
            .is_err()
        );
        for source in [
            "stdout",
            "stderr",
            "connector",
            "slurm_batch",
            "model.solver",
            &format!("model.{}", "a".repeat(64)),
        ] {
            let decoded: LogSource = serde_json::from_value(serde_json::json!(source)).unwrap();
            assert_eq!(decoded.as_str(), source);
            assert_eq!(
                serde_json::to_value(decoded).unwrap(),
                serde_json::json!(source)
            );
        }
        for source in [
            "model.",
            "model.Upper",
            "model.a/b",
            "model.1abc",
            "model.a.b",
            &format!("model.{}", "a".repeat(65)),
        ] {
            assert!(serde_json::from_value::<LogSource>(serde_json::json!(source)).is_err());
        }
        let mut files = BTreeMap::new();
        for n in 0..=MAX_MODEL_LOG_FILES {
            files.insert(
                format!("s{n}"),
                LogFileDeclaration {
                    path: format!("logs/{n}.log"),
                },
            );
            assert_eq!(validate_log_files(&files).is_ok(), n < MAX_MODEL_LOG_FILES);
        }
    }

    #[test]
    fn additional_sources_share_fixed_live_and_archive_budgets() {
        for slurm in [false, true] {
            let base = if slurm { 3 } else { 2 };
            for named in 0..=MAX_MODEL_LOG_FILES {
                let count = base + named;
                let policy = LogPolicy {
                    segment_bytes: 4096,
                    retained_segments: 128,
                    archive_source_bytes: 1024 * 1024,
                    ..Default::default()
                };
                let effective = policy.for_sources(count, slurm);
                assert_eq!(effective.retained_segments, policy.retained_segments);
                assert!(effective.live_bytes() * count as u64 <= policy.live_bytes() * base as u64);
                assert!(
                    effective.archive_source_bytes * count as u64
                        <= policy.archive_source_bytes * base as u64
                );
                assert!(effective.archive_source_bytes >= effective.live_bytes());
            }
        }
    }

    #[test]
    fn policy_bounds_include_compression_overhead() {
        let mut p = LogPolicy::default();
        assert!(p.validate().is_ok());
        p.archive_compressed_bytes = p.archive_source_bytes * 3;
        assert!(p.validate().is_err());
        p = LogPolicy::default();
        p.retained_segments = u32::MAX;
        assert!(p.validate().is_err());
    }
}
