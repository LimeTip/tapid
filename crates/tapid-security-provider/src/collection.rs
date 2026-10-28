use std::{cell::Cell, io::Read};
use tapid_attestations::{EvidenceFreshness, SecurityTrust, VerifiedSecurityEvidenceV2};
use tapid_protocol::{SecurityRequest, SecurityStatus};

use crate::{Disclosure, RetrievalError, retrieve_recorded};

/// Maximum processed occurrences, including denied or failed requests. Callers
/// should apply this limit before loading per-request state or asking consent.
pub const MAX_COLLECTION_REQUESTS: usize = 1000;

/// Exact request authorization and independently loaded issuer/request replay floor.
pub struct CollectionInput<'a> {
    pub request: &'a SecurityRequest,
    pub disclosure: Disclosure<'a>,
    pub freshness: &'a EvidenceFreshness<'a>,
}

/// Results retain input order, including failures. Findings in successful entries
/// remain available when a different occurrence cannot be evaluated.
pub struct SecurityCollection {
    pub results: Vec<Result<VerifiedSecurityEvidenceV2, RetrievalError>>,
    pub not_checked: usize,
    pub limit: Option<CollectionLimit>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionLimit {
    Requests,
    ResponseBytes,
}

impl SecurityCollection {
    /// Evidence completeness only; the caller must also establish graph coverage.
    /// An empty request list cannot establish that a project has been checked.
    pub fn complete(&self) -> bool {
        self.not_checked == 0
            && !self.results.is_empty()
            && self.results.iter().all(|result| {
                result.as_ref().is_ok_and(|evidence| {
                    matches!(
                        evidence.result().status,
                        SecurityStatus::Findings | SecurityStatus::NoKnownFindings
                    ) && evidence.result().limitations.is_empty()
                })
            })
    }
}

/// Collect signed evidence without stopping at the first provider failure.
/// Every successful entry passes the same durable recording gate as a single
/// retrieval. The caller retains ownership of requests and their graph mapping.
pub fn collect_recorded<R: Read>(
    inputs: &[CollectionInput<'_>],
    trust: &SecurityTrust<'_>,
    transport: &mut impl FnMut(&SecurityRequest) -> Result<R, RetrievalError>,
    record: &mut impl FnMut(&str, &SecurityRequest, u64, &str) -> Result<(), RetrievalError>,
) -> SecurityCollection {
    let remaining = Cell::new(32 * 1024 * 1024);
    let mut results = Vec::new();
    for input in inputs.iter().take(MAX_COLLECTION_REQUESTS) {
        if remaining.get() == 0 {
            break;
        }
        results.push(retrieve_recorded(
            input.request,
            &input.disclosure,
            trust,
            input.freshness,
            &mut |request| {
                transport(request).map(|stream| BudgetedRead {
                    stream,
                    remaining: &remaining,
                })
            },
            record,
        ));
    }
    SecurityCollection {
        not_checked: inputs.len() - results.len(),
        limit: if remaining.get() == 0 {
            Some(CollectionLimit::ResponseBytes)
        } else if inputs.len() > results.len() {
            Some(CollectionLimit::Requests)
        } else {
            None
        },
        results,
    }
}

struct BudgetedRead<'a, R> {
    stream: R,
    remaining: &'a Cell<usize>,
}

impl<R: Read> Read for BudgetedRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let limit = buffer.len().min(self.remaining.get());
        if limit == 0 {
            return Err(std::io::Error::other("security response budget exhausted"));
        }
        let read = self.stream.read(&mut buffer[..limit])?;
        self.remaining.set(self.remaining.get() - read);
        Ok(read)
    }
}
