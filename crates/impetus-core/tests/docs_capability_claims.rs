//! Docs capability claims vs `CapabilityTruthReport` (issue #167).
//!
//! Fixture: `tests/fixtures/docs_capability_claims.json` (redacted, no secrets).
//! Re-run: `cargo test -p impetus-core --test docs_capability_claims`

use impetus_core::{CapabilityLevel, CapabilityTruthReport};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct DocsCapabilityClaims {
    schema_version: u16,
    registered_providers: Vec<String>,
    claims: Vec<Claim>,
}

#[derive(Debug, Deserialize)]
struct Claim {
    id: String,
    level: CapabilityLevel,
    #[serde(default)]
    details: Value,
}

fn load_claims() -> DocsCapabilityClaims {
    let raw = include_str!("fixtures/docs_capability_claims.json");
    assert!(
        !raw.contains("sk-") && !raw.contains("Bearer ") && !raw.contains("api_key"),
        "docs capability claims fixture must not contain secrets"
    );
    serde_json::from_str(raw).expect("docs_capability_claims.json parses")
}

fn detail_subset_matches(expected: &Value, actual: &Value) -> bool {
    match expected {
        Value::Object(want) => {
            let Some(got) = actual.as_object() else {
                return false;
            };
            want.iter().all(|(key, value)| got.get(key) == Some(value))
        }
        Value::Null => true,
        other => actual == other,
    }
}

#[test]
fn selected_docs_claims_match_capability_truth_report() {
    let fixture = load_claims();
    assert_eq!(fixture.schema_version, 1);

    let report = CapabilityTruthReport::gather(&fixture.registered_providers);
    assert_eq!(report.schema_version, fixture.schema_version);

    for claim in &fixture.claims {
        let entry = report.entry(&claim.id).unwrap_or_else(|| {
            panic!(
                "missing capability id `{}` in CapabilityTruthReport",
                claim.id
            )
        });
        assert_eq!(
            entry.level, claim.level,
            "capability `{}` level drifted (docs claim vs CapabilityTruthReport)",
            claim.id
        );
        let actual_details = entry.details.as_ref().unwrap_or(&Value::Null);
        assert!(
            detail_subset_matches(&claim.details, actual_details),
            "capability `{}` details drifted\nclaim: {}\nreport: {}",
            claim.id,
            claim.details,
            actual_details
        );
    }
}

#[test]
fn docs_claims_fixture_has_no_secret_shaped_fields() {
    let raw = include_str!("fixtures/docs_capability_claims.json");
    let value: Value = serde_json::from_str(raw).expect("json");
    let blob = value.to_string();
    assert!(!blob.contains("sk-"));
    assert!(!blob.contains("Bearer "));
    assert!(!blob.to_lowercase().contains("\"password\""));
    assert!(!blob.to_lowercase().contains("\"token\""));
}
