use tapid_security_provider::{CredentialMode, ProviderConfig, RetrievalError};

fn request() -> tapid_protocol::SecurityRequest {
    serde_json::from_value(
        serde_json::from_slice::<serde_json::Value>(include_bytes!("fixtures/advisory-v2.json"))
            .unwrap()["claims"]["request"]
            .clone(),
    )
    .unwrap()
}

#[test]
fn credentials_never_authorize_inventory_disclosure() {
    let config = ProviderConfig::parse(
        br#"{"version":1,"origin":"https://cloud.example","accountCredentialEnv":"AUDIT_ACCOUNT","headlessCredentialEnv":"AUDIT_CI","cache":"disabled"}"#,
    ).unwrap();
    let request = request();
    let mut lookups = 0;
    let result = config.authorize(
        &request,
        &[],
        CredentialMode::Account,
        "https://cloud.example",
        &mut |_| {
            lookups += 1;
            Some("fixture-token".into())
        },
    );
    assert!(matches!(result, Err(RetrievalError::NotAuthorized)));
    assert_eq!(lookups, 0);
    let allowed = [request.clone()];
    let token = config
        .authorize(
            &request,
            &allowed,
            CredentialMode::Headless,
            "https://cloud.example",
            &mut |name| {
                assert_eq!(name, "AUDIT_CI");
                Some("fixture-ci-token".into())
            },
        )
        .unwrap();
    assert_eq!(token.as_deref(), Some("fixture-ci-token"));
    let mut changed = request.clone();
    changed.subject.requested_name = "another".into();
    assert!(matches!(
        config.authorize(
            &changed,
            &allowed,
            CredentialMode::Headless,
            "https://cloud.example",
            &mut |_| panic!("changed inventory must not read credentials"),
        ),
        Err(RetrievalError::NotAuthorized)
    ));
}

#[test]
fn configuration_rejects_ambiguous_origins_cache_policy_and_credential_sources() {
    let valid = serde_json::json!({"version":1,"origin":"https://cloud.example","accountCredentialEnv":"AUDIT_ACCOUNT","headlessCredentialEnv":"AUDIT_CI","cache":"disabled"});
    for (field, value) in [
        ("version", serde_json::json!(2)),
        ("origin", serde_json::json!("http://cloud.example")),
        ("origin", serde_json::json!("https://cloud.example/path")),
        (
            "origin",
            serde_json::json!("https://user:secret@cloud.example"),
        ),
        ("origin", serde_json::json!("https://CLOUD.example:443/")),
        ("accountCredentialEnv", serde_json::json!("bad\r\nname")),
        ("headlessCredentialEnv", serde_json::json!("AUDIT_ACCOUNT")),
        ("cache", serde_json::json!("enabled")),
        ("unknown", serde_json::json!(true)),
    ] {
        let mut document = valid.clone();
        document[field] = value;
        assert!(
            ProviderConfig::parse(&serde_json::to_vec(&document).unwrap()).is_err(),
            "{field}"
        );
    }
    let mut missing_policy = valid.clone();
    missing_policy.as_object_mut().unwrap().remove("cache");
    assert!(ProviderConfig::parse(&serde_json::to_vec(&missing_policy).unwrap()).is_err());
    assert!(ProviderConfig::parse(&vec![b' '; 65537]).is_err());
    assert!(
        ProviderConfig::parse(
            br#"{"version":1,"version":1,"origin":"https://cloud.example","cache":"disabled"}"#
        )
        .is_err()
    );
}

#[test]
fn authorization_never_falls_back_or_leaks_credentials_to_another_origin() {
    let config = ProviderConfig::parse(br#"{"version":1,"origin":"https://cloud.example","accountCredentialEnv":"AUDIT_ACCOUNT","cache":"disabled"}"#).unwrap();
    let request = request();
    let allowed = [request.clone()];
    for destination in [
        "https://other.example",
        "https://cloud.example/",
        "http://cloud.example",
        "https://cloud.example:443",
    ] {
        assert!(matches!(
            config.authorize(
                &request,
                &allowed,
                CredentialMode::Account,
                destination,
                &mut |_| panic!("wrong origin")
            ),
            Err(RetrievalError::NotAuthorized)
        ));
    }
    assert!(matches!(
        config.authorize(
            &request,
            &allowed,
            CredentialMode::Headless,
            "https://cloud.example",
            &mut |_| panic!("no fallback")
        ),
        Err(RetrievalError::NotAuthorized)
    ));
    for token in [
        None,
        Some(""),
        Some("secret\r\nHeader:value"),
        Some("a=b"),
        Some("with space"),
    ] {
        assert!(matches!(
            config.authorize(
                &request,
                &allowed,
                CredentialMode::Account,
                "https://cloud.example",
                &mut |_| token.map(str::to_owned)
            ),
            Err(RetrievalError::NotAuthorized)
        ));
    }
    assert_eq!(
        config
            .authorize(
                &request,
                &allowed,
                CredentialMode::Anonymous,
                "https://cloud.example",
                &mut |_| panic!("anonymous")
            )
            .unwrap(),
        None
    );
    let mut private = request;
    private.audience = tapid_protocol::EvidenceAudience::Tenant {
        tenant: "fixture-tenant".into(),
    };
    assert!(matches!(
        config.authorize(
            &private,
            &[private.clone()],
            CredentialMode::Anonymous,
            "https://cloud.example",
            &mut |_| panic!("anonymous private")
        ),
        Err(RetrievalError::NotAuthorized)
    ));
}

#[test]
fn credential_sources_remain_distinct_on_case_insensitive_environments() {
    assert!(ProviderConfig::parse(br#"{"version":1,"origin":"https://cloud.example","accountCredentialEnv":"AUDIT_TOKEN","headlessCredentialEnv":"audit_token","cache":"disabled"}"#).is_err());
}
