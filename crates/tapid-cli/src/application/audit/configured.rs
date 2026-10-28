//! Explicit live activation. No project discovery, browser session, registry
//! credentials or default issuer keys participate in this path.
use std::path::Path;
use tapid_security_provider::{CredentialMode, ProviderConfig};

use super::AuditReport;
use crate::filesystem::read::read_bounded_file;

#[cfg(all(test, unix))]
mod tests;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrustDocument {
    version: u32,
    issuer: String,
    valid_until: u64,
    max_age: u64,
    keyring: tapid_signatures::EmbeddedKeyRing,
}

fn external_path(project: &Path, path: &Path) -> Result<(), &'static str> {
    let project = project
        .canonicalize()
        .map_err(|_| "AUDIT_PROJECT_UNREADABLE")?;
    let resolved = path
        .canonicalize()
        .map_err(|_| "AUDIT_PROVIDER_INPUT_UNREADABLE")?;
    if resolved.starts_with(project) {
        return Err("AUDIT_PROVIDER_INPUT_IN_PROJECT");
    }
    Ok(())
}

pub(crate) fn inspect_configured(
    project: &Path,
    config_path: &Path,
    consent_path: &Path,
    trust_path: &Path,
    state_path: &Path,
    mode: CredentialMode,
) -> Result<AuditReport, &'static str> {
    inspect_configured_using(
        project,
        config_path,
        consent_path,
        trust_path,
        state_path,
        mode,
        |config, consented, mode| {
            let https = tapid_security_provider::HttpsProvider::new(
                config,
                std::time::Duration::from_secs(15),
                std::time::Duration::from_secs(60),
            )
            .map_err(|_| "AUDIT_PROVIDER_UNAVAILABLE")?;
            Ok(Box::new(move |request| {
                https
                    .query(request, &consented, mode, &mut |name| {
                        std::env::var(name).ok()
                    })
                    .map(|response| Box::new(response) as Box<dyn std::io::Read>)
            }))
        },
    )
}

// Keep the external byte-stream boundary injectable while using the same input
// validation, consent, signature verification and durable recording in tests.
fn inspect_configured_using(
    project: &Path,
    config_path: &Path,
    consent_path: &Path,
    trust_path: &Path,
    state_path: &Path,
    mode: CredentialMode,
    make_transport: impl FnOnce(
        ProviderConfig,
        Vec<tapid_protocol::SecurityRequest>,
        CredentialMode,
    ) -> Result<Box<super::collection::Transport<'static>>, &'static str>,
) -> Result<AuditReport, &'static str> {
    for path in [config_path, consent_path, trust_path] {
        external_path(project, path)?;
    }
    let config = ProviderConfig::parse(
        &read_bounded_file(config_path, 64 * 1024).map_err(|_| "AUDIT_PROVIDER_CONFIG_INVALID")?,
    )
    .map_err(|_| "AUDIT_PROVIDER_CONFIG_INVALID")?;
    let consented = config
        .parse_consent(
            &read_bounded_file(consent_path, 1024 * 1024).map_err(|_| "AUDIT_CONSENT_INVALID")?,
        )
        .map_err(|_| "AUDIT_CONSENT_INVALID")?;
    let trust: TrustDocument = serde_json::from_slice(
        &read_bounded_file(trust_path, 64 * 1024).map_err(|_| "AUDIT_TRUST_INVALID")?,
    )
    .map_err(|_| "AUDIT_TRUST_INVALID")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "AUDIT_CLOCK_INVALID")?
        .as_secs();
    if trust.version != 1
        || trust.issuer.is_empty()
        || trust.issuer.len() > 2048
        || trust.issuer.chars().any(char::is_control)
        || trust.valid_until <= now
        || trust.max_age == 0
    {
        return Err("AUDIT_TRUST_INVALID");
    }
    let keys = tapid_signatures::KeyRing::from_embedded_json(
        &serde_json::to_vec(&trust.keyring).map_err(|_| "AUDIT_TRUST_INVALID")?,
    )
    .map_err(|_| "AUDIT_TRUST_INVALID")?;
    if keys.is_empty() {
        return Err("AUDIT_TRUST_INVALID");
    }
    // Validate inputs before opening security state or constructing transport.
    super::inspect(project)?;
    let parent = state_path.parent().ok_or("AUDIT_REPLAY_STATE_INVALID")?;
    external_path(project, parent)?;
    if state_path.exists() {
        external_path(project, state_path)?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (mode, config, consented, keys, make_transport);
        Err("AUDIT_REPLAY_PLATFORM_UNSUPPORTED")
    }
    #[cfg(any(unix, windows))]
    {
        use tapid_security_provider::{ReplayStore, RetrievalError};
        let replay = ReplayStore::open(state_path).map_err(|_| "AUDIT_REPLAY_STATE_INVALID")?;
        let mut transport = make_transport(config, consented.clone(), mode)?;
        let mut freshness =
            |_: &tapid_protocol::SecurityRequest| tapid_attestations::EvidenceFreshness {
                now,
                max_age: trust.max_age,
                minimum_sequence: 0,
                previous_digest: None,
            };
        let mut authorize = |request: &tapid_protocol::SecurityRequest| consented.contains(request);
        let mut record = |_: &str, _: &tapid_protocol::SecurityRequest, _: u64, _: &str| {
            Err(RetrievalError::StateUnavailable)
        };
        let mut provider = super::Provider {
            trust: tapid_attestations::SecurityTrust {
                issuer: &trust.issuer,
                keys: &keys,
                valid_until: trust.valid_until,
            },
            freshness: &mut freshness,
            authorize: &mut authorize,
            transport: &mut transport,
            record: &mut record,
            replay: Some(&replay),
            threshold: tapid_policy::AuditThreshold::Any,
        };
        super::inspect_with_provider(project, Some(&mut provider))
    }
}
