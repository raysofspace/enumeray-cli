//! Bounded immutable collections. Member paths are data, never storage locators.
use crate::models::{self, ContractError, SchemaKind, ScientificDefinition};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};
use unicode_normalization::{UnicodeNormalization, is_nfc};
use uuid::Uuid;

pub const MAX_MEMBERS: usize = 10_000;
pub const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PAGE_BYTES: usize = 64 * 1024;

/// Closed immutable collection reference used by scientific inputs and results.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionReference {
    #[serde(deserialize_with = "crate::models::files::canonical_reference_id")]
    #[cfg_attr(
        feature = "openapi",
        schema(
            pattern = "^(?!00000000-0000-0000-0000-000000000000$)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
        )
    )]
    pub collection_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidCollection;
impl fmt::Display for InvalidCollection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid or conflicting collection manifest")
    }
}
impl std::error::Error for InvalidCollection {}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionDeclaration {
    pub media_types: Vec<String>,
    pub max_members: u32,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}
impl CollectionDeclaration {
    pub fn permits(&self, members: &[MemberSpec]) -> bool {
        members.len() <= self.max_members as usize
            && members.iter().all(|m| {
                m.size_bytes <= self.max_file_bytes && self.media_types.contains(&m.media_type)
            })
            && total_bytes(members).is_ok_and(|n| n <= self.max_total_bytes)
    }
}

pub fn declarations(
    definition: &ScientificDefinition,
    output: bool,
) -> Result<BTreeMap<String, CollectionDeclaration>, ContractError> {
    if definition.contract_version != 1 {
        return Err(ContractError::UnsupportedContractVersion);
    }
    let root = if output {
        &definition.output_schema
    } else {
        &definition.input_schema
    };
    let kind = if output {
        SchemaKind::Output
    } else {
        SchemaKind::Input
    };
    let invalid = || ContractError::InvalidFileDeclarations;
    let mut schemas = BTreeMap::new();
    models::contract::collect_schemas(root, String::new(), kind, &mut schemas)?;
    let mut result = BTreeMap::new();
    for (pointer, schema) in &schemas {
        let Some(annotation) = schema.get("x-enumeray-collection") else {
            continue;
        };
        let name = root["properties"]
            .as_object()
            .and_then(|p| {
                p.keys()
                    .find(|name| *pointer == models::contract::join_pointer("/properties", name))
            })
            .ok_or_else(invalid)?;
        let declaration: CollectionDeclaration =
            serde_json::from_value(annotation.clone()).map_err(|_| invalid())?;
        if schema.get("x-enumeray-file").is_some()
            || declaration.media_types.is_empty()
            || declaration.media_types.len() > 16
            || declaration.max_members as usize > MAX_MEMBERS
            || declaration.max_file_bytes > models::MAX_ARTIFACT_BYTES
            || declaration.max_total_bytes > i64::MAX as u64
            || declaration
                .media_types
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != declaration.media_types.len()
            || declaration
                .media_types
                .iter()
                .any(|m| !crate::media::valid_media_type(m))
            || schema["type"] != "object"
            || schema["additionalProperties"] != false
            || schema["required"] != json!(["collection_id"])
            || schema["properties"]
                .as_object()
                .is_none_or(|p| p.len() != 1)
            || schema["properties"]["collection_id"]["type"] != "string"
        {
            return Err(invalid());
        }
        result.insert(name.clone(), declaration);
    }
    if !result.is_empty() {
        models::contract::validate_output_declarations(root).map_err(|_| invalid())?;
        if schemas.values().any(|s| s.get("$ref").is_some()) {
            return Err(invalid());
        }
    }
    Ok(result)
}

pub fn reference(value: &Value) -> Option<Uuid> {
    if value.as_object()?.len() != 1 {
        return None;
    }
    let text = value["collection_id"].as_str()?;
    let id = Uuid::parse_str(text).ok()?;
    (!id.is_nil() && id.to_string() == text).then_some(id)
}
pub fn references(
    definition: &ScientificDefinition,
    inputs: &Value,
) -> Result<Vec<(String, Uuid, CollectionDeclaration)>, models::InputError> {
    let mut refs = Vec::new();
    for (name, decl) in
        declarations(definition, false).map_err(models::InputError::InvalidContract)?
    {
        if let Some(value) = inputs.get(&name) {
            refs.push((
                name,
                reference(value).ok_or(models::InputError::InvalidCollectionReference)?,
                decl,
            ));
        }
    }
    refs.sort_by_key(|r| r.1);
    Ok(refs)
}
pub fn requires(definition: &ScientificDefinition, output: bool) -> bool {
    let root = if output {
        &definition.output_schema
    } else {
        &definition.input_schema
    };
    root["properties"]
        .as_object()
        .is_some_and(|p| p.values().any(|s| s.get("x-enumeray-collection").is_some()))
}

/// Portable lexical path; do not normalize aliases before checking uniqueness.
pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').count() <= 32
        && path.len() <= 1024
        && is_nfc(path)
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.len() <= 255
                && p.trim() == p
                && !p.ends_with('.')
                && !p.starts_with(".enumeray-download-")
        })
}
fn empty_metadata() -> Value {
    json!({})
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeclaredMember {
    pub path: String,
    pub media_type: String,
    #[serde(default = "empty_metadata")]
    pub scientific_metadata: Value,
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemberSpec {
    pub path: String,
    pub media_type: String,
    pub scientific_metadata: Value,
    pub size_bytes: u64,
    #[cfg_attr(
        feature = "openapi",
        schema(
            min_length = 44,
            max_length = 44,
            pattern = "^[A-Za-z0-9+/]{42}[AEIMQUYcgkosw048]=$"
        )
    )]
    pub checksum_sha256: String,
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedMember {
    #[serde(flatten)]
    pub member: MemberSpec,
    pub artifact_id: Uuid,
    pub version_id: String,
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CollectionRequest {
    pub logical_name: String,
    pub scientific_metadata: Value,
    pub member_count: u32,
    pub total_bytes: u64,
    #[cfg_attr(
        feature = "openapi",
        schema(
            min_length = 44,
            max_length = 44,
            pattern = "^[A-Za-z0-9+/]{42}[AEIMQUYcgkosw048]=$"
        )
    )]
    pub manifest_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_name: Option<String>,
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
pub struct CollectionReceipt {
    pub collection_id: Uuid,
    pub request: CollectionRequest,
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
pub struct CollectionView {
    #[serde(flatten)]
    pub receipt: CollectionReceipt,
    pub state: String,
    #[cfg_attr(
        feature = "openapi",
        schema(
            required = true,
            min_length = 44,
            max_length = 44,
            pattern = "^[A-Za-z0-9+/]{42}[AEIMQUYcgkosw048]=$"
        )
    )]
    pub manifest_digest: Option<String>,
    /// Current tracked member availability. Delivery independently verifies the
    /// exact storage version; this flag never substitutes for content evidence.
    pub available: bool,
}
/// Compact listing; full scientific metadata remains on the collection itself.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
pub struct CollectionSummary {
    pub collection_id: Uuid,
    pub state: String,
    pub manifest_digest: Option<String>,
    pub member_count: u32,
    pub total_bytes: u64,
    pub available: bool,
}
impl From<CollectionView> for CollectionSummary {
    fn from(view: CollectionView) -> Self {
        Self {
            collection_id: view.receipt.collection_id,
            state: view.state,
            manifest_digest: view.manifest_digest,
            member_count: view.receipt.request.member_count,
            total_bytes: view.receipt.request.total_bytes,
            available: view.available,
        }
    }
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
pub struct MemberView {
    pub index: u32,
    pub member_id: Uuid,
    pub spec: MemberSpec,
    pub artifact_id: Option<Uuid>,
    pub version_id: Option<String>,
    pub available: bool,
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
pub struct MemberPage {
    pub items: Vec<MemberView>,
    pub next_cursor: Option<u32>,
}

/// Retained identity and exact range accepted by a member metadata page.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
pub struct CollectionPageReceipt {
    pub collection_id: Uuid,
    pub start: u32,
    pub count: usize,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
pub struct CollectionFreezeReceipt {
    pub collection_id: Uuid,
    #[cfg_attr(
        feature = "openapi",
        schema(
            min_length = 44,
            max_length = 44,
            pattern = "^[A-Za-z0-9+/]{42}[AEIMQUYcgkosw048]=$"
        )
    )]
    pub manifest_sha256: String,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
pub struct CollectionBindingReceipt {
    pub artifact_id: Uuid,
    pub version_id: String,
}

pub fn parse_document<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    limit: usize,
) -> Result<T, InvalidCollection> {
    if bytes.len() > limit {
        return Err(InvalidCollection);
    }
    models::validation::preflight_inputs(bytes).map_err(|_| InvalidCollection)?;
    serde_json::from_slice(bytes).map_err(|_| InvalidCollection)
}
pub fn valid_checksum(s: &str) -> bool {
    STANDARD
        .decode(s)
        .is_ok_and(|b| b.len() == 32 && STANDARD.encode(b) == s)
}
pub fn total_bytes(members: &[MemberSpec]) -> Result<u64, InvalidCollection> {
    members.iter().try_fold(0u64, |sum, m| {
        sum.checked_add(m.size_bytes).ok_or(InvalidCollection)
    })
}
fn path_alias(path: &str) -> String {
    // Include case expansions and alternate lowercase forms, conservatively
    // rejecting aliases across case-insensitive filesystems before staging.
    path.chars()
        .flat_map(char::to_lowercase)
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .nfc()
        .collect()
}
pub fn validate_members(members: &[MemberSpec]) -> Result<(), InvalidCollection> {
    if members.len() > MAX_MEMBERS {
        return Err(InvalidCollection);
    }
    let mut paths = BTreeSet::new();
    let mut folded = BTreeSet::new();
    let mut previous: Option<&str> = None;
    for m in members {
        if !valid_path(&m.path)
            || !crate::media::valid_media_type(&m.media_type)
            || m.size_bytes > models::MAX_ARTIFACT_BYTES
            || !valid_checksum(&m.checksum_sha256)
            || serde_json::to_vec(m).map_or(true, |b| b.len() > MAX_PAGE_BYTES - 2048)
            || !m.scientific_metadata.is_object()
            || models::validate_input_value(&m.scientific_metadata).is_err()
            || previous.is_some_and(|p| p >= m.path.as_str())
            || !folded.insert(path_alias(&m.path))
        {
            return Err(InvalidCollection);
        }
        for (i, _) in m.path.match_indices('/') {
            if paths.contains(&m.path[..i]) || folded.contains(&path_alias(&m.path[..i])) {
                return Err(InvalidCollection);
            }
        }
        let prefix = format!("{}/", path_alias(&m.path));
        if folded
            .range(prefix.clone()..)
            .next()
            .is_some_and(|p| p.starts_with(&prefix))
        {
            return Err(InvalidCollection);
        }
        paths.insert(m.path.as_str());
        previous = Some(&m.path);
    }
    if serde_json::to_vec(members)
        .map_err(|_| InvalidCollection)?
        .len()
        > MAX_MANIFEST_BYTES
    {
        return Err(InvalidCollection);
    }
    total_bytes(members)?;
    Ok(())
}
pub fn manifest_digest<T: Serialize + ?Sized>(members: &T) -> Result<String, InvalidCollection> {
    let bytes = serde_json::to_vec(members).map_err(|_| InvalidCollection)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(InvalidCollection);
    }
    Ok(STANDARD.encode(Sha256::digest(bytes)))
}
pub fn member_id(collection: Uuid, index: u32) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"enumeray-collection-member-v1");
    hash.update(collection.as_bytes());
    hash.update(index.to_be_bytes());
    let bytes = hash.finalize();
    Uuid::from_bytes(bytes[..16].try_into().expect("digest"))
}

#[cfg(test)]
mod public_identity_vectors {
    use super::*;

    #[test]
    fn independent_downloadable_vectors_match_runtime_serialization() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/http-contract/digest-vectors");
        let catalogue: Value =
            serde_json::from_slice(&std::fs::read(root.join("vectors.json")).unwrap()).unwrap();
        assert_eq!(catalogue["schema_version"], 1);
        for vector in catalogue["vectors"].as_array().unwrap() {
            let source = std::fs::read(root.join(vector["input"].as_str().unwrap())).unwrap();
            let expected = std::fs::read(root.join(vector["bytes"].as_str().unwrap())).unwrap();
            let (actual, digest) = if vector["sealed"] == true {
                let members: Vec<PinnedMember> =
                    parse_document(&source, MAX_MANIFEST_BYTES).unwrap();
                let specs = members.iter().map(|m| m.member.clone()).collect::<Vec<_>>();
                validate_members(&specs).unwrap();
                (
                    serde_json::to_vec(&members).unwrap(),
                    manifest_digest(&members).unwrap(),
                )
            } else {
                let members: Vec<MemberSpec> = parse_document(&source, MAX_MANIFEST_BYTES).unwrap();
                validate_members(&members).unwrap();
                (
                    serde_json::to_vec(&members).unwrap(),
                    manifest_digest(&members).unwrap(),
                )
            };
            assert_eq!(actual, expected, "{}", vector["name"]);
            assert_eq!(digest, vector["sha256_base64"].as_str().unwrap());
            assert_eq!(actual.len() as u64, vector["size_bytes"].as_u64().unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn member(path: &str) -> MemberSpec {
        MemberSpec {
            path: path.into(),
            media_type: "application/octet-stream".into(),
            scientific_metadata: json!({}),
            size_bytes: 0,
            checksum_sha256: STANDARD.encode(Sha256::digest([])),
        }
    }
    #[test]
    fn paths_and_aliases_are_rejected_before_normalization() {
        for p in [
            "",
            "/a",
            "../a",
            "a/../b",
            "a/./b",
            "a//b",
            "a/",
            "C:/a",
            "a\\b",
            "a\0b",
            "a. ",
            "a.",
            "cafe\u{301}",
            "nested/.enumeray-download-checkpoint/state.json",
        ] {
            assert!(!valid_path(p), "{p:?}");
        }
        for paths in [
            vec!["a", "a"],
            vec!["a", "a/b"],
            vec!["b", "a"],
            vec!["A", "a"],
            vec!["A/b", "a"],
        ] {
            assert!(validate_members(&paths.into_iter().map(member).collect::<Vec<_>>()).is_err());
        }
        assert!(validate_members(&[member("a/b"), member("c")]).is_ok());
        assert!(validate_members(&[member("caf\u{e9}/r\u{e9}sultat")]).is_ok());
        assert!(validate_members(&[member("CAF\u{c9}"), member("caf\u{e9}")]).is_err());
        assert!(validate_members(&[member("\u{3a3}"), member("\u{3c2}")]).is_err());
        assert!(validate_members(&[member("\u{df}"), member("\u{1e9e}")]).is_err());
    }
    #[test]
    fn count_and_manifest_digest_are_bounded() {
        assert!(validate_members(&[]).is_ok());
        let mut m = (0..MAX_MEMBERS)
            .map(|n| member(&format!("{n:05}")))
            .collect::<Vec<_>>();
        validate_members(&m).unwrap();
        let digest = manifest_digest(&m).unwrap();
        assert!(valid_checksum(&digest));
        m.push(member("99999"));
        assert!(validate_members(&m).is_err());
        assert_eq!(member_id(Uuid::nil(), 1), member_id(Uuid::nil(), 1));
        assert_ne!(member_id(Uuid::nil(), 1), member_id(Uuid::nil(), 2));
    }
    #[test]
    fn manifest_byte_bound_is_independent_of_member_count_and_page_bound() {
        let members = (0..1000)
            .map(|n| {
                let mut m = member(&format!("{n:05}"));
                m.scientific_metadata = json!({"description":"x".repeat(17000)});
                m
            })
            .collect::<Vec<_>>();
        assert!(serde_json::to_vec(&members[0]).unwrap().len() < MAX_PAGE_BYTES);
        assert!(validate_members(&members).is_err());
        assert!(manifest_digest(&members).is_err());
    }
}

#[cfg(test)]
mod declaration_tests {
    use super::*;
    fn fixture_model() -> crate::models::ParsedPublication {
        crate::models::parse_publication(include_bytes!(
            "../../../examples/collections/publication.json"
        ))
        .unwrap()
    }
    #[test]
    fn declaration_references_and_capabilities_are_explicit() {
        let model = fixture_model();
        crate::models::contract::validate_contract(&model.definition).unwrap();
        let id = Uuid::new_v4();
        crate::models::validate_inputs(&model.definition, &json!({"dataset":{"collection_id":id}}))
            .unwrap();
        for bad in [
            json!({"dataset":{"artifact_id":id}}),
            json!({"dataset":{"collection_id":id,"path":"local"}}),
            json!({"dataset":{"collection_id":id.to_string().to_uppercase()}}),
        ] {
            assert!(crate::models::validate_inputs(&model.definition, &bad).is_err());
        }
        let required = crate::execution::ExecutionRequirements::derive(
            crate::execution::ExecutionDriver::Process,
            &model.definition,
        );
        assert!(
            required
                .capabilities
                .contains(&crate::execution::Capability::CollectionInputs)
        );
        assert!(
            required
                .capabilities
                .contains(&crate::execution::Capability::CollectionOutputs)
        );
        assert!(
            required
                .capabilities
                .contains(&crate::execution::Capability::LargeArtifacts)
        );
    }
    #[test]
    fn manifest_preserves_flat_pins_and_rejects_duplicate_json() {
        let m = MemberSpec {
            path: "a".into(),
            media_type: "text/plain".into(),
            scientific_metadata: json!({}),
            size_bytes: 0,
            checksum_sha256: STANDARD.encode(Sha256::digest([])),
        };
        let p = PinnedMember {
            member: m,
            artifact_id: Uuid::new_v4(),
            version_id: "v1".into(),
        };
        let bytes = serde_json::to_vec(&p).unwrap();
        let decoded: PinnedMember = parse_document(&bytes, MAX_PAGE_BYTES).unwrap();
        assert_eq!(decoded.member.path, "a");
        assert!(parse_document::<Value>(br#"{"path":"a","path":"b"}"#, MAX_PAGE_BYTES).is_err());
        assert!(
            parse_document::<Value>(br#"{"scientific_metadata":{"a":1,"a":2}}"#, MAX_PAGE_BYTES)
                .is_err()
        );
    }
    #[test]
    fn optional_collection_slots_can_report_failure_without_a_reference() {
        let mut model = fixture_model();
        model.definition.output_schema["required"] = json!(["member_count"]);
        let report = json!({"attempt_id":Uuid::new_v4(),"outcome":"succeeded","execution_terminated":true,"outputs":{"member_count":0},"files":{},"optional_output_failures":{"snapshots":{"code":"not_produced","message":"No snapshots for this calculation."}},"failure":null});
        let request =
            crate::assignments::results::parse_request(&serde_json::to_vec(&report).unwrap())
                .unwrap();
        crate::assignments::results::validate_outputs(&model.definition, &request).unwrap();
        model.definition.output_schema["required"] = json!(["snapshots", "member_count"]);
        assert!(
            crate::assignments::results::validate_outputs(&model.definition, &request).is_err()
        );
    }
    #[test]
    fn collection_annotations_cannot_hide_in_nested_or_referenced_schemas() {
        let mut model = fixture_model();
        model.definition.input_schema["properties"]["dataset"]["x-enumeray-file"] =
            json!({"max_bytes":1,"media_types":["text/plain"]});
        assert!(crate::models::contract::validate_contract(&model.definition).is_err());
        let mut model = fixture_model();
        model.definition.input_schema["properties"]["alias"] =
            json!({"$ref":"#/properties/dataset"});
        assert!(crate::models::contract::validate_contract(&model.definition).is_err());
        let mut model = fixture_model();
        model.definition.input_schema["properties"]["dataset"]["x-enumeray-collection"]["max_members"] =
            json!(10001);
        assert!(crate::models::contract::validate_contract(&model.definition).is_err());
    }
}
