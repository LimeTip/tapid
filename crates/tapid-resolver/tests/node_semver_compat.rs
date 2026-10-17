use serde_json::Value;
use tapid_core::PackageVersion;
use tapid_resolver::Requirement;

const CASES: &str = include_str!("../../../tests/node-semver-oracle/cases.json");

#[test]
fn requirement_satisfaction_matches_pinned_node_semver_corpus() {
    let cases: Vec<Value> = serde_json::from_str(CASES).expect("valid shared semver corpus");
    assert!(
        !cases.is_empty(),
        "semver differential corpus must not be empty"
    );

    for (index, case) in cases.iter().enumerate() {
        let range = case["range"].as_str().expect("range string");
        let version_text = case["version"].as_str().expect("version string");
        let expected_match = case["satisfies"].as_bool().expect("satisfaction boolean");
        let expected_valid = case
            .get("validRange")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let parsed = range.parse::<Requirement>();
        assert_eq!(
            parsed
                .as_ref()
                .is_ok_and(|requirement| requirement.dist_tag().is_none()),
            expected_valid,
            "case {index}: range validity for {range:?}"
        );
        if !expected_valid {
            continue;
        }

        let version = match version_text.parse::<PackageVersion>() {
            Ok(version) => version,
            Err(_) => {
                assert!(!expected_match, "case {index}: invalid version matched");
                continue;
            }
        };
        assert_eq!(
            parsed.expect("validated range").matches(&version),
            expected_match,
            "case {index}: {version_text:?} against {range:?}"
        );
    }
}
