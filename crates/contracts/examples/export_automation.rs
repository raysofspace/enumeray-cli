//! Emit independent JSON Schema documents from the actual CLI projection types.
use enumeray_contracts::automation::{
    AnalysisCollectionSummary, AnalysisSummary, CliRuntimeError, ReleaseSummary,
};
use serde_json::{Value, json};
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(components(schemas(
    AnalysisSummary,
    AnalysisCollectionSummary,
    ReleaseSummary,
    CliRuntimeError
)))]
struct AutomationSchemas;

fn references(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(reference)) = object.get_mut("$ref") {
                *reference = reference.replace("#/components/schemas/", "#/$defs/");
            }
            for child in object.values_mut() {
                references(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                references(child);
            }
        }
        _ => {}
    }
}

fn main() {
    let document = serde_json::to_value(AutomationSchemas::openapi()).expect("OpenAPI schemas");
    let mut definitions = document["components"]["schemas"].clone();
    references(&mut definitions);
    let mut documents = serde_json::Map::new();
    for (name, root) in [
        ("analysis-summary", "AnalysisSummary"),
        ("analysis-collection-summary", "AnalysisCollectionSummary"),
        ("release-summary", "ReleaseSummary"),
        ("runtime-error", "CliRuntimeError"),
    ] {
        documents.insert(
            name.into(),
            json!({
                "$schema":"https://json-schema.org/draft/2020-12/schema",
                "$id":format!("https://doc.enumeray.com/contracts/cli-output-v1/{name}.json"),
                "$ref":format!("#/$defs/{root}"), "$defs":definitions,
            }),
        );
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&documents).expect("JSON Schema documents")
    );
}
