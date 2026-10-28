use tapid_protocol::{
    AccountAuthorization, SECURITY_CONTRACT_V1, SecurityPermission, SecurityReason,
};

fn claims() -> AccountAuthorization {
    AccountAuthorization {
        version: SECURITY_CONTRACT_V1.into(),
        authority: "https://accounts.example".into(),
        subject: "user-1".into(),
        tenant: "org-1".into(),
        audience: "https://cloud.example".into(),
        scopes: vec![SecurityPermission::ReadPrivate],
        issued_at: 1000,
        expires_at: 2000,
        credential_id: "credential-1".into(),
    }
}

#[test]
fn account_claims_require_supported_version_before_other_validation() {
    let mut value = claims();
    assert_eq!(value.validate(), Ok(()));
    value.version = "future".into();
    value.subject.clear();
    assert_eq!(value.validate(), Err(SecurityReason::UnsupportedVersion));
}

#[test]
fn account_claims_enforce_identity_scope_and_time_bounds() {
    let mut value = claims();
    value.scopes.clear();
    assert_eq!(value.validate(), Ok(()));
    for field in ["subject", "tenant", "credentialId"] {
        for (text, valid) in [
            ("".into(), false),
            (" \t".into(), false),
            ("a\nb".into(), false),
            ("a".repeat(4096), true),
            ("a".repeat(4097), false),
            ("é".repeat(2048), true),
            ("é".repeat(2049), false),
        ] {
            let mut value = claims();
            match field {
                "subject" => value.subject = text,
                "tenant" => value.tenant = text,
                _ => value.credential_id = text,
            }
            assert_eq!(
                value.validate(),
                if valid {
                    Ok(())
                } else {
                    Err(SecurityReason::InvalidRequest)
                },
                "{field}"
            );
        }
    }
    for field in ["authority", "audience"] {
        for origin in [
            "",
            "http://cloud.example",
            "https://cloud.example/",
            "https://user:pass@cloud.example",
            "https://cloud.example/path",
            "https://CLOUD.example",
            "https://cloud.example:443",
            "file:///archive",
        ] {
            let mut value = claims();
            if field == "authority" {
                value.authority = origin.into();
            } else {
                value.audience = origin.into();
            }
            assert_eq!(
                value.validate(),
                Err(SecurityReason::InvalidRequest),
                "{field}: {origin}"
            );
        }
    }
    value = claims();
    value.scopes.push(SecurityPermission::ReadPrivate);
    assert_eq!(value.validate(), Err(SecurityReason::InvalidRequest));
    for (issued, expires, valid) in [
        (0, 1, true),
        (1, 1, false),
        (2, 1, false),
        (9_007_199_254_740_990, 9_007_199_254_740_991, true),
        (1, 9_007_199_254_740_992, false),
        (9_007_199_254_740_992, 9_007_199_254_740_993, false),
    ] {
        value = claims();
        value.issued_at = issued;
        value.expires_at = expires;
        assert_eq!(
            value.validate(),
            if valid {
                Ok(())
            } else {
                Err(SecurityReason::InvalidRequest)
            }
        );
    }
}
