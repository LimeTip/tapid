//! Per-invocation reporting, independent of selection and materialization work.
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug)]
pub(crate) enum AcquisitionSource {
    Local,
    Network,
}

#[derive(Clone, Debug)]
pub(crate) struct AcquiredArchive {
    pub bytes: Vec<u8>,
    pub source: AcquisitionSource,
}

#[derive(Default)]
pub(crate) struct ArtifactEvidence {
    cached: BTreeSet<String>,
    acquired_trees: BTreeSet<String>,
    downloaded: BTreeSet<String>,
    local: BTreeSet<String>,
}
impl ArtifactEvidence {
    pub(crate) fn cached(&mut self, tree: &tapid_core::ArtifactDigest) {
        self.cached.insert(tree.to_string());
    }
    pub(crate) fn acquired(
        &mut self,
        archive: &tapid_core::ArtifactDigest,
        tree: &tapid_core::ArtifactDigest,
        source: AcquisitionSource,
    ) {
        self.acquired_trees.insert(tree.to_string());
        self.verified_archive(archive, source);
    }
    // Copied archives can be verified during resolution even when preparation
    // subsequently accepts a pre-existing tree. That does not acquire a new tree.
    pub(crate) fn verified_archive(
        &mut self,
        archive: &tapid_core::ArtifactDigest,
        source: AcquisitionSource,
    ) {
        match source {
            AcquisitionSource::Local => &mut self.local,
            AcquisitionSource::Network => &mut self.downloaded,
        }
        .insert(archive.to_string());
    }
    pub(crate) fn merge(&mut self, other: Self) {
        self.cached.extend(other.cached);
        self.acquired_trees.extend(other.acquired_trees);
        self.downloaded.extend(other.downloaded);
        self.local.extend(other.local);
    }
    pub(crate) fn finish(self, baseline: Option<BTreeSet<String>>) -> ArtifactStatistics {
        let Some(baseline) = baseline else {
            return ArtifactStatistics::Unavailable {
                reason: "initial_store_baseline_unavailable",
            };
        };
        ArtifactStatistics::Available {
            cached_trees_reused: self
                .cached
                .intersection(&baseline)
                .filter(|tree| !self.acquired_trees.contains(*tree))
                .count(),
            downloaded_artifacts_verified: self.downloaded.len(),
            local_artifacts_verified: self.local.len(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub(crate) enum ArtifactStatistics {
    Available {
        cached_trees_reused: usize,
        downloaded_artifacts_verified: usize,
        local_artifacts_verified: usize,
    },
    Unavailable {
        reason: &'static str,
    },
}
impl std::fmt::Display for ArtifactStatistics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available {
                cached_trees_reused: cached,
                downloaded_artifacts_verified: downloaded,
                local_artifacts_verified: local,
            } => {
                let trees = if *cached == 1 { "tree" } else { "trees" };
                let archives = |count| if count == 1 { "archive" } else { "archives" };
                write!(
                    f,
                    "Artifacts: {cached} cached {trees} reused; {downloaded} downloaded {} verified; {local} local {} verified.",
                    archives(*downloaded),
                    archives(*local)
                )
            }
            Self::Unavailable { .. } => write!(
                f,
                "Artifacts: statistics unavailable (initial store baseline unavailable)."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_evidence_deduplicates_each_source_and_excludes_acquired_or_late_trees() {
        let tree = |digit: &str| format!("sha256-{}", digit.repeat(64)).parse().unwrap();
        let existing = tree("1");
        let acquired = tree("2");
        let late = tree("3");
        let archive = tree("4");
        let second_archive = tree("5");
        let mut evidence = ArtifactEvidence::default();
        evidence.cached(&existing);
        evidence.cached(&existing);
        evidence.cached(&late);
        evidence.cached(&acquired);
        let mut hydrated = ArtifactEvidence::default();
        for _ in 0..3 {
            hydrated.acquired(&archive, &acquired, AcquisitionSource::Network);
        }
        hydrated.acquired(&archive, &acquired, AcquisitionSource::Local);
        hydrated.acquired(&second_archive, &acquired, AcquisitionSource::Local);
        evidence.merge(hydrated);
        let statistics = evidence.finish(Some([existing.to_string(), acquired.to_string()].into()));
        assert_eq!(
            serde_json::to_value(statistics).unwrap(),
            serde_json::json!({
                "availability":"available", "cached_trees_reused":1,
                "downloaded_artifacts_verified":1, "local_artifacts_verified":2
            })
        );
        assert_eq!(
            serde_json::to_value(ArtifactEvidence::default().finish(None)).unwrap(),
            serde_json::json!({
                "availability":"unavailable", "reason":"initial_store_baseline_unavailable"
            })
        );
    }
}
