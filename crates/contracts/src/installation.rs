//! Read-only installation comparison receipts. Identity agreement is not site or scientific qualification.
use crate::automation::SchemaVersion;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum CompatibilityStatus {
    Match,
    Mismatch,
    MissingEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum CompatibilityTrust {
    SignedRelease,
    UnsignedDevelopment,
    AuthenticatedTransfer,
    Unverified,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CompatibilityCheck {
    pub component: String,
    pub field: String,
    pub status: CompatibilityStatus,
    pub expected: Value,
    pub observed: Value,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CompatibilityReceipt {
    pub schema_version: SchemaVersion,
    pub status: CompatibilityStatus,
    pub trust: CompatibilityTrust,
    pub observed_at: String,
    /// Exact selected identity and evidence hashes; not a component registry.
    pub selected: Value,
    pub checks: Vec<CompatibilityCheck>,
    pub limits: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn saved_receipt_requires_known_schema_status_trust_and_complete_structure() {
        let valid = json!({"schema_version":1,"status":"missing_evidence","trust":"unsigned_development","observed_at":"2026-11-02T00:00:00Z","selected":{},"checks":[{"component":"service","field":"build","status":"missing_evidence","expected":null,"observed":null,"reason":"not observed"}],"limits":["No release qualification."]});
        let receipt: CompatibilityReceipt = serde_json::from_value(valid.clone()).unwrap();
        assert_eq!(receipt.status, CompatibilityStatus::MissingEvidence);
        assert_eq!(receipt.trust, CompatibilityTrust::UnsignedDevelopment);
        for (field, value) in [
            ("schema_version", json!(2)),
            ("schema_version", json!(true)),
            ("status", json!("verified")),
            ("trust", json!("trusted")),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(
                serde_json::from_value::<CompatibilityReceipt>(invalid).is_err(),
                "{field}"
            );
        }
        for field in [
            "schema_version",
            "status",
            "trust",
            "checks",
            "limits",
            "selected",
            "observed_at",
        ] {
            let mut incomplete = valid.clone();
            incomplete.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<CompatibilityReceipt>(incomplete).is_err(),
                "{field}"
            );
        }
    }
}
