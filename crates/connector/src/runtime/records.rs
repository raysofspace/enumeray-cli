//! Immutable typed views over original journal evidence.
//!
//! Serialization writes the original object, preserving additive fields and
//! absent legacy fields. The decoded projection is rebuilt and checked on read;
//! callers cannot mutate either representation independently.

use super::{Result, failure};
use enumeray_contracts::{
    assignments::observations::EventDisposition, assignments::results::ResultOutcome,
    runs::RunPhase,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::Value;
use std::ops::Deref;
use uuid::Uuid;

#[derive(Clone)]
pub struct Record<T> {
    original: Value,
    decoded: T,
}

impl<T: DeserializeOwned> Record<T> {
    pub fn decode(original: Value) -> Result<Self> {
        // Deserialize from bytes: RawValue-backed fields cannot be faithfully
        // deserialized using Value's synthetic arbitrary-precision representation.
        let bytes =
            serde_json::to_vec(&original).map_err(|_| failure("invalid evidence record"))?;
        let decoded =
            serde_json::from_slice(&bytes).map_err(|_| failure("invalid evidence record"))?;
        Ok(Self { original, decoded })
    }
}

impl<T> Record<T> {
    pub fn decoded(&self) -> &T {
        &self.decoded
    }
}

impl<T> Deref for Record<T> {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.original
    }
}

impl<T> PartialEq<Value> for Record<T> {
    fn eq(&self, other: &Value) -> bool {
        self.original == *other
    }
}

impl<T> Serialize for Record<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.original.serialize(serializer)
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Record<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::decode(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

// Journal projections deliberately contain only the fields used for local
// decisions. Wire request DTOs are stricter than historical journal validators:
// using them here would reject old evidence or erase additive fields on save.
#[derive(Clone, Deserialize)]
pub struct ReportIdentity {
    pub attempt_id: Uuid,
    pub execution_terminated: bool,
}

#[derive(Clone, Deserialize)]
pub struct ResultEvidence {
    pub receipt: TerminalReceipt,
}

#[derive(Clone, Deserialize)]
pub struct ReceiptOutcome {
    pub outcome: ResultOutcome,
    #[serde(default)]
    pub provider_cleanup_acknowledged: bool,
}

#[derive(Clone, Deserialize)]
pub struct EventIdentity {
    pub attempt_id: Uuid,
    pub sequence: i64,
    pub phase: Option<RunPhase>,
}

// Historical journals did not validate saved event acknowledgements. Decode
// their useful fields opportunistically; a new HTTP acknowledgement must still
// match every expected identity before it can be recorded.
#[derive(Clone, Deserialize)]
#[serde(from = "Value")]
pub struct EventAcknowledgement {
    pub assignment_id: Option<Uuid>,
    pub event_id: Option<Uuid>,
    pub sequence: Option<i64>,
    pub phase: Option<RunPhase>,
    pub disposition: Option<EventDisposition>,
}

impl From<Value> for EventAcknowledgement {
    fn from(value: Value) -> Self {
        Self {
            assignment_id: value["assignment_id"]
                .as_str()
                .and_then(|id| id.parse().ok()),
            event_id: value["event_id"].as_str().and_then(|id| id.parse().ok()),
            sequence: value["sequence"].as_i64(),
            phase: value["phase"]
                .as_str()
                .and_then(|phase| serde_json::from_value(Value::from(phase)).ok()),
            disposition: value["disposition"]
                .as_str()
                .and_then(|disposition| serde_json::from_value(Value::from(disposition)).ok()),
        }
    }
}

pub type Assignment = Record<enumeray_contracts::assignments::claims::AssignmentReceipt>;
pub type TerminalResult = Record<ResultEvidence>;
pub type Resolution = Record<enumeray_contracts::assignments::resolution::ResolutionSummary>;
pub type TerminalReport = Record<ReportIdentity>;
pub type TerminalReceipt = Record<ReceiptOutcome>;
pub type EventRequest = Record<EventIdentity>;
pub type EventReceipt = Record<EventAcknowledgement>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_receipt_and_additive_fields_round_trip_without_normalization() {
        let original = json!({
            "assignment_id":"f39b17e5-01e5-46df-81ab-84faefbc4cb7",
            "run_id":"b2b0f090-2d75-4e15-9f26-8c723f2fb2a7",
            "attempt_id":"16611c52-692f-4653-9eb2-f9a2ee3d702d",
            "outcome":"succeeded", "completed_at":"2026-09-21T12:00:00Z",
            "future_evidence":{"retained":true}
        });
        let record = TerminalReceipt::decode(original.clone()).unwrap();
        assert!(record.get("provider_cleanup_acknowledged").is_none());
        assert_eq!(serde_json::to_value(&record).unwrap(), original);
        let restored: TerminalReceipt =
            serde_json::from_slice(&serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), original);
    }

    #[test]
    fn exact_terminal_values_survive_typed_projection_and_reopening() {
        let original: Value = serde_json::from_str(
            r#"{
            "attempt_id":"16611c52-692f-4653-9eb2-f9a2ee3d702d",
            "outcome":"succeeded","execution_terminated":true,
            "outputs":{"large":9007199254740993.1,"small":1e-1000},
            "files":{},"optional_output_failures":{},"failure":null
        }"#,
        )
        .unwrap();
        let record = TerminalReport::decode(original.clone()).unwrap();
        assert_eq!(record["outputs"], original["outputs"]);
        assert_eq!(serde_json::to_value(&record).unwrap(), original);
        let restored: TerminalReport =
            serde_json::from_slice(&serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(restored["outputs"], original["outputs"]);
    }

    #[test]
    fn event_acknowledgements_require_string_discriminants_for_http_matching() {
        let original = json!({"phase":{"executing":null}, "disposition":{"applied":null}});
        let receipt = EventReceipt::decode(original.clone()).unwrap();
        assert_eq!(receipt.decoded().phase, None);
        assert_eq!(receipt.decoded().disposition, None);
        // Retained legacy evidence is still preserved even if it cannot be used
        // to acknowledge a new HTTP request.
        assert_eq!(serde_json::to_value(receipt).unwrap(), original);
    }

    #[test]
    fn assignment_projection_preserves_the_previous_uuid_parser_and_exact_inputs() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let mut assignment = fixture["prepared_assignments"][0]["assignment"].clone();
        assignment["inputs"] =
            serde_json::from_str(r#"{"large":9007199254740993.1,"small":1e-1000}"#).unwrap();
        for identity in [
            json!("F39B17E5-01E5-46DF-81AB-84FAEFBC4CB7"),
            json!("f39b17e501e546df81ab84faefbc4cb7"),
            json!("{f39b17e5-01e5-46df-81ab-84faefbc4cb7}"),
            json!("urn:uuid:f39b17e5-01e5-46df-81ab-84faefbc4cb7"),
            json!("wrong"),
            json!(42),
            Value::Null,
        ] {
            assignment["assignment_id"] = identity;
            let previous = serde_json::from_value::<
                enumeray_contracts::assignments::claims::AssignmentReceipt,
            >(assignment.clone());
            let current = Assignment::decode(assignment.clone());
            assert_eq!(previous.is_ok(), current.is_ok());
            if let (Ok(previous), Ok(current)) = (previous, current) {
                assert_eq!(previous.assignment_id, current.decoded().assignment_id);
                assert_eq!(previous.inputs, current.decoded().inputs);
                assert_eq!(serde_json::to_value(current).unwrap(), assignment);
            }
        }
    }

    #[test]
    fn malformed_terminal_discriminant_is_rejected_before_entering_the_journal() {
        assert!(TerminalReceipt::decode(json!({"outcome":"unknown"})).is_err());
    }
}
