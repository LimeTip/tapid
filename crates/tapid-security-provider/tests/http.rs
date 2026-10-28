use std::time::Duration;
use tapid_security_provider::{CredentialMode, HttpsProvider, ProviderConfig, RetrievalError};

#[test]
fn https_queries_require_consent_before_credentials_or_transport() {
    let config = ProviderConfig::parse(br#"{"version":1,"origin":"https://cloud.example","headlessCredentialEnv":"AUDIT_CI","cache":"disabled"}"#).unwrap();
    let provider =
        HttpsProvider::new(config, Duration::from_secs(2), Duration::from_secs(5)).unwrap();
    let request = serde_json::from_value(
        serde_json::from_slice::<serde_json::Value>(include_bytes!("fixtures/advisory-v2.json"))
            .unwrap()["claims"]["request"]
            .clone(),
    )
    .unwrap();
    assert!(matches!(
        provider.query(&request, &[], CredentialMode::Headless, &mut |_| panic!(
            "no consent"
        )),
        Err(RetrievalError::NotAuthorized)
    ));
}

#[test]
fn expired_collection_never_consults_credentials() {
    let config = ProviderConfig::parse(br#"{"version":1,"origin":"https://cloud.example","headlessCredentialEnv":"AUDIT_CI","cache":"disabled"}"#).unwrap();
    let provider =
        HttpsProvider::new(config, Duration::from_secs(2), Duration::from_nanos(1)).unwrap();
    let request = serde_json::from_value(
        serde_json::from_slice::<serde_json::Value>(include_bytes!("fixtures/advisory-v2.json"))
            .unwrap()["claims"]["request"]
            .clone(),
    )
    .unwrap();
    assert!(matches!(
        provider.query(
            &request,
            std::slice::from_ref(&request),
            CredentialMode::Headless,
            &mut |_| panic!("expired collection")
        ),
        Err(RetrievalError::Unavailable)
    ));
}

#[test]
fn invalid_deadlines_cannot_disable_transport_limits() {
    for (request, collection) in [
        (Duration::ZERO, Duration::from_secs(1)),
        (Duration::from_secs(1), Duration::ZERO),
        (Duration::from_secs(301), Duration::from_secs(1)),
        (Duration::from_secs(1), Duration::MAX),
    ] {
        let config = ProviderConfig::parse(
            br#"{"version":1,"origin":"https://cloud.example","cache":"disabled"}"#,
        )
        .unwrap();
        assert!(matches!(
            HttpsProvider::new(config, request, collection),
            Err(RetrievalError::Unavailable)
        ));
    }
}
