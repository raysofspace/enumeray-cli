//! Personal-token wire contracts. Secret issuance is deliberately not Debug.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Vocabulary accepted by token issuance; scopes never replace resource roles.
pub const PERSONAL_TOKEN_SCOPES: &[&str] = &[
    "me:read",
    "models:read",
    "models:manage",
    "runs:create",
    "runs:read",
    "runs:cancel",
    "uploads:create",
    "uploads:read",
    "uploads:complete",
    "artifacts:read",
    "artifacts:download",
    "connectors:read",
    "connectors:manage",
    "access:manage",
    "tokens:manage",
    "audit:read",
    "service:read",
    "service:manage",
];

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateToken {
    /// 1–200 UTF-8 bytes without surrounding whitespace or control characters.
    #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 200))]
    pub name: String,
    #[cfg_attr(feature = "openapi", schema(schema_with = token_scopes_schema))]
    pub scopes: Vec<String>,
    #[cfg_attr(feature = "openapi", schema(minimum = 1, maximum = 31536000))]
    pub expires_in_seconds: u64,
}

/// Delivered exactly once. Store privately and never print to diagnostic logs.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TokenIssued {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token: String,
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub expires_at: String,
    #[cfg_attr(feature = "openapi", schema(schema_with = token_scopes_schema))]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TokenInfo {
    pub id: Uuid,
    pub name: String,
    /// Inventory also includes wildcard operator credentials; delegated issuance
    /// cannot request that wildcard authority.
    #[cfg_attr(feature = "openapi", schema(schema_with = inventory_scopes_schema))]
    pub scopes: Vec<String>,
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub created_at: String,
    #[cfg_attr(feature = "openapi", schema(required = true, format = DateTime))]
    #[serde(deserialize_with = "present")]
    pub expires_at: Option<String>,
    #[cfg_attr(feature = "openapi", schema(required = true, format = DateTime))]
    #[serde(deserialize_with = "present")]
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TokenPage {
    pub items: Vec<TokenInfo>,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    #[serde(deserialize_with = "present")]
    pub next_cursor: Option<Uuid>,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(feature = "openapi")]
fn token_scopes_schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    serde_json::from_value(serde_json::json!({
        "type":"array", "minItems":1, "maxItems":PERSONAL_TOKEN_SCOPES.len(),
        "items":{"type":"string", "enum":PERSONAL_TOKEN_SCOPES}
    }))
    .expect("personal token scope schema")
}

#[cfg(feature = "openapi")]
fn inventory_scopes_schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    let scopes = PERSONAL_TOKEN_SCOPES
        .iter()
        .copied()
        .chain(["*"])
        .collect::<Vec<_>>();
    serde_json::from_value(serde_json::json!({
        "type":"array", "minItems":1, "maxItems":32,
        "items":{"type":"string", "enum":scopes},
        "description":"Current retained scopes, including '*' for operator-issued credentials. Delegated issuance never accepts '*'."
    })).expect("retained credential scope schema")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn token_inventory_requires_explicit_cursor_and_never_needs_a_secret() {
        let page = json!({"items":[],"next_cursor":null,"future_addition":true});
        assert!(serde_json::from_value::<TokenPage>(page).is_ok());
        assert!(serde_json::from_value::<TokenPage>(json!({"items":[]})).is_err());
    }
}
