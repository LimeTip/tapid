use tapid_policy::{Decision, SecurityPolicy};

#[test]
fn all_required_security_categories_deny_without_evidence() {
    for rule in [
        serde_json::json!({"id":"malware", "kind":"malware"}),
        serde_json::json!({"id":"vulnerabilities", "kind":"vulnerability", "threshold":"high"}),
        serde_json::json!({"id":"analysis", "kind":"content_analysis", "profile":"standard", "revision":"1"}),
    ] {
        let mut rule = rule;
        for (key, value) in serde_json::json!({
            "mode":"required", "sources":["provider"], "max-age-seconds":60, "status":"cached"
        })
        .as_object()
        .unwrap()
        {
            rule[key] = value.clone();
        }
        let policy: SecurityPolicy =
            serde_json::from_value(serde_json::json!({"version":1, "rules":[rule]})).unwrap();
        let result = policy.unavailable_evidence().unwrap();
        assert_eq!(result.decision, Decision::Deny);
        assert_eq!(result.reason, "POLICY_SECURITY_EVIDENCE_UNAVAILABLE");
    }
}

fn malware_rule(mode: &str) -> serde_json::Value {
    serde_json::json!({"id":"malware", "kind":"malware", "mode":mode,
        "sources":["provider"], "max-age-seconds":60, "status":"cached"})
}

#[test]
fn advisory_evidence_does_not_satisfy_a_required_rule() {
    let advisory = malware_rule("advisory");
    let policy: SecurityPolicy =
        serde_json::from_value(serde_json::json!({"version":1,"rules":[advisory.clone()]}))
            .unwrap();
    assert_eq!(
        policy.unavailable_evidence().unwrap().decision,
        Decision::Warn
    );
    let mut required = malware_rule("required");
    required["id"] = "required-malware".into();
    let policy: SecurityPolicy =
        serde_json::from_value(serde_json::json!({"version":1,"rules":[advisory,required]}))
            .unwrap();
    assert_eq!(
        policy.unavailable_evidence().unwrap().decision,
        Decision::Deny
    );
}

#[test]
fn invalid_requirements_cannot_be_evaluated() {
    let base = serde_json::json!({"version":1,"rules":[malware_rule("required")]});
    let mut invalid = Vec::new();
    for (pointer, value) in [
        ("/version", serde_json::json!(2)),
        ("/rules/0/max-age-seconds", serde_json::json!(0)),
        ("/rules/0/id", serde_json::json!("")),
        ("/rules/0/sources", serde_json::json!([])),
        (
            "/rules/0/sources",
            serde_json::json!(["provider", "provider"]),
        ),
        ("/rules/0/threshold", serde_json::json!("high")),
    ] {
        let mut value_under_test = base.clone();
        if pointer == "/rules/0/threshold" {
            value_under_test["rules"][0]["threshold"] = value;
        } else {
            *value_under_test.pointer_mut(pointer).unwrap() = value;
        }
        invalid.push(value_under_test);
    }
    invalid.push(serde_json::json!({"version":1,"rules":[malware_rule("required"),malware_rule("required")]}));
    for value in invalid {
        let policy: SecurityPolicy = serde_json::from_value(value).unwrap();
        assert!(policy.unavailable_evidence().is_err());
    }
    let mut unknown = base;
    unknown["rules"][0]["ignore-errors"] = true.into();
    assert!(serde_json::from_value::<SecurityPolicy>(unknown).is_err());
}
