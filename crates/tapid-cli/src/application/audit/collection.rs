use std::io::Read;
use tapid_attestations::{EvidenceFreshness, SecurityTrust};
use tapid_policy::{AuditOutcome, AuditThreshold, evaluate_audit};
use tapid_protocol::{SecurityRequest, SecurityResultV2};
use tapid_security_provider::{CollectionInput, Disclosure, RetrievalError, collect_recorded};

use super::requests::Inventory;

type Transport<'a> = dyn FnMut(&SecurityRequest) -> Result<Box<dyn Read>, RetrievalError> + 'a;
type Recorder<'a> = dyn FnMut(&str, &SecurityRequest, u64, &str) -> Result<(), RetrievalError> + 'a;

/// Internal injection seam. Authorization is exact-request disclosure consent,
/// independent of credentials. Replay floors must be loaded per issuer/request;
/// record must durably compare/publish against concurrent writers.
pub(crate) struct Provider<'a> {
    pub(crate) trust: SecurityTrust<'a>,
    pub(crate) freshness: &'a mut dyn FnMut(&SecurityRequest) -> EvidenceFreshness<'a>,
    pub(crate) authorize: &'a mut dyn FnMut(&SecurityRequest) -> bool,
    pub(crate) transport: &'a mut Transport<'a>,
    pub(crate) record: &'a mut Recorder<'a>,
    /// When present, owns replay loading and recording; injected freshness still
    /// supplies time/age policy and may impose a stricter floor.
    #[cfg(unix)]
    pub(crate) replay: Option<&'a tapid_security_provider::ReplayStore>,
    pub(crate) threshold: AuditThreshold,
}

#[derive(serde::Serialize)]
pub(crate) struct AuditFailure {
    pub(crate) key: String,
    pub(crate) code: &'static str,
    pub(crate) reason: Option<tapid_protocol::SecurityReason>,
}

pub(crate) struct AuditEvidence {
    pub(crate) key: String,
    pub(crate) result: SecurityResultV2,
    pub(crate) paths: Vec<tapid_lockfile::LockedGraphPath>,
    pub(crate) paths_truncated: bool,
}

pub(super) fn collect(
    inventory: &Inventory,
    graph_complete: bool,
    provider: Option<&mut Provider<'_>>,
) -> (Vec<AuditEvidence>, AuditOutcome, Vec<AuditFailure>, usize) {
    let Some(provider) = provider else {
        return (
            Vec::new(),
            AuditOutcome::Incomplete,
            Vec::new(),
            inventory.requests.len(),
        );
    };
    #[cfg(unix)]
    let floors = inventory
        .requests
        .iter()
        .map(|(_, request)| {
            provider
                .replay
                .map_or(Ok(None), |store| store.load(provider.trust.issuer, request))
        })
        .collect::<Vec<_>>();
    let freshness = inventory
        .requests
        .iter()
        .map(|(_, request)| (provider.freshness)(request))
        .collect::<Vec<_>>();
    #[cfg(unix)]
    let state_failed = freshness
        .iter()
        .zip(&floors)
        .map(|(freshness, floor)| match floor {
            Err(_) => true,
            Ok(Some((sequence, digest))) if *sequence == freshness.minimum_sequence => freshness
                .previous_digest
                .is_some_and(|previous| previous != digest),
            _ => false,
        })
        .collect::<Vec<_>>();
    #[cfg(unix)]
    let freshness = {
        let mut freshness = freshness;
        for (freshness, floor) in freshness.iter_mut().zip(&floors) {
            if let Ok(Some((sequence, digest))) = floor
                && *sequence >= freshness.minimum_sequence
            {
                freshness.minimum_sequence = *sequence;
                freshness.previous_digest = Some(digest);
            }
        }
        freshness
    };
    let inputs = inventory
        .requests
        .iter()
        .zip(&freshness)
        .map(|((_, request), freshness)| CollectionInput {
            request,
            disclosure: if (provider.authorize)(request) {
                Disclosure::Exact(request)
            } else {
                Disclosure::Denied
            },
            freshness,
        })
        .collect::<Vec<_>>();
    let collection = collect_recorded(
        &inputs,
        &provider.trust,
        &mut |request| {
            #[cfg(unix)]
            if let Some(index) = inventory
                .requests
                .iter()
                .position(|(_, input)| input == request)
                && state_failed[index]
            {
                return Err(RetrievalError::StateUnavailable);
            }
            (provider.transport)(request)
        },
        &mut |issuer, request, sequence, digest| {
            #[cfg(unix)]
            if let Some(store) = provider.replay {
                return store.record(issuer, request, sequence, digest);
            }
            (provider.record)(issuer, request, sequence, digest)
        },
    );
    let complete = graph_complete && inventory.excluded == 0 && collection.complete();
    let mut failures = Vec::new();
    let evidence = inventory
        .requests
        .iter()
        .zip(collection.results)
        .filter_map(|((key, _), result)| match result {
            Ok(evidence) => Some(AuditEvidence {
                key: key.clone(),
                result: evidence.result().clone(),
                paths: Vec::new(),
                paths_truncated: false,
            }),
            Err(error) => {
                failures.push(AuditFailure {
                    key: key.clone(),
                    reason: match error {
                        RetrievalError::Evidence(reason) => Some(reason),
                        _ => None,
                    },
                    code: match error {
                        RetrievalError::NotAuthorized => "not_authorized",
                        RetrievalError::Unavailable => "unavailable",
                        RetrievalError::StateUnavailable => "state_unavailable",
                        RetrievalError::Evidence(tapid_protocol::SecurityReason::Expired) => {
                            "stale"
                        }
                        RetrievalError::Evidence(_) => "invalid_evidence",
                    },
                });
                None
            }
        })
        .collect::<Vec<_>>();
    let outcome = evaluate_audit(
        evidence.iter().flat_map(|e| &e.result.findings),
        complete,
        provider.threshold,
    );
    (evidence, outcome, failures, collection.not_checked)
}
