//! Bounded evidence sampled from canonical normalization state at return points.
use super::NormalizedRecords;
use crate::application::outcome::{
    CandidateExclusions, ExcludedCandidate, ExcludedPackage, OperationalError,
};

// Select by full canonical cache keys. Display identities never participate in grouping.
const MAX_EXCLUSION_PACKAGES: usize = 16;
const MAX_EXCLUSIONS_PER_PACKAGE: usize = 8;
const MAX_EXCLUSIONS: usize = 100;
const MAX_EXCLUSION_FIELD_BYTES: usize = 256;
const MAX_EXCLUSION_SUMMARY_BYTES: usize = 2048;

fn display_identity(value: &str) -> (String, bool) {
    let mut text = crate::application::outcome::sanitize(value);
    text.retain(|c| !c.is_control());
    let truncated = text.len() > MAX_EXCLUSION_FIELD_BYTES;
    if truncated {
        let mut end = MAX_EXCLUSION_FIELD_BYTES - 3;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...");
    }
    (text, truncated)
}

pub(super) fn candidate_exclusions(
    normalized: &NormalizedRecords,
    package: Option<(&str, &str)>,
) -> CandidateExclusions {
    let mut result = CandidateExclusions::default();
    let mut previous = None;
    let mut group_index = None;
    let mut retained = 0;
    for ((source, name, version), record) in normalized {
        let Err(reason) = record else { continue };
        if package.is_some_and(|key| key != (source.as_str(), name.as_str())) {
            continue;
        }
        let key = (source, name);
        if previous != Some(key) {
            previous = Some(key);
            result.total_packages += 1;
            group_index = None;
            if result.packages.len() < MAX_EXCLUSION_PACKAGES && retained < MAX_EXCLUSIONS {
                let (source, source_truncated) = display_identity(source);
                let (name, name_truncated) = display_identity(name);
                let mut fields = Vec::new();
                if source_truncated {
                    fields.push("source");
                }
                if name_truncated {
                    fields.push("name");
                }
                group_index = Some(result.packages.len());
                result.packages.push(ExcludedPackage {
                    source,
                    name,
                    total_candidates: 0,
                    omitted_candidates: 0,
                    truncated: !fields.is_empty(),
                    truncated_fields: fields,
                    candidates: Vec::new(),
                });
            }
        }
        result.total_candidates += 1;
        if let Some(index) = group_index {
            let group = &mut result.packages[index];
            group.total_candidates += 1;
            if group.candidates.len() < MAX_EXCLUSIONS_PER_PACKAGE && retained < MAX_EXCLUSIONS {
                let (version, truncated) = display_identity(version);
                group.truncated |= truncated;
                group.candidates.push(ExcludedCandidate {
                    version,
                    reason_code: reason.reason,
                    dependency_category: reason.category,
                    truncated_fields: if truncated {
                        vec!["version"]
                    } else {
                        Vec::new()
                    },
                });
                retained += 1;
            } else {
                group.omitted_candidates += 1;
                group.truncated = true;
            }
        }
    }
    result.omitted_packages = result.total_packages - result.packages.len();
    result.omitted_candidates = result.total_candidates - retained;
    result.truncated = result.omitted_packages > 0
        || result.omitted_candidates > 0
        || result.packages.iter().any(|group| group.truncated);
    result
}

pub(super) fn with_exclusions(
    mut error: OperationalError,
    normalized: &NormalizedRecords,
    registry: &str,
    name: &str,
) -> OperationalError {
    let evidence = candidate_exclusions(normalized, Some((registry, name)));
    if evidence.is_empty() {
        return error;
    }
    let group = &evidence.packages[0];
    let mut summary = format!(
        "excluded {} observed candidates for {}:{}",
        group.total_candidates, group.source, group.name
    );
    let mut shown = 0;
    for candidate in &group.candidates {
        let entry = format!("; {} ({})", candidate.version, candidate.reason_code.code());
        // Reserve space for the omission notice; append only complete entries.
        if summary.len() + entry.len() + 80 > MAX_EXCLUSION_SUMMARY_BYTES {
            break;
        }
        summary.push_str(&entry);
        shown += 1;
    }
    summary.push_str(&format!("; {} omitted", group.total_candidates - shown));
    error = error.context(summary);
    let context = error.diagnostic.get_or_insert_with(Default::default);
    if context.package.is_none() {
        context.package = Some(crate::application::outcome::DiagnosticPackage {
            source: registry.to_owned(),
            name: name.to_owned(),
            version: None,
        });
    }
    context.candidate_exclusions = evidence;
    error
}

#[cfg(test)]
mod exclusion_display_tests {
    use super::display_identity;

    #[test]
    fn exclusion_identity_display_redacts_before_utf8_truncation() {
        let input = format!(
            "\u{001b}https://user:password@example.test/?token=secret#fragment {}",
            "界".repeat(200)
        );
        let (display, truncated) = display_identity(&input);
        assert!(truncated);
        assert!(display.len() <= 256);
        assert!(display.ends_with("..."));
        assert!(!display.chars().any(char::is_control));
        for secret in ["user", "password", "secret", "fragment"] {
            assert!(!display.contains(secret));
        }
        let json = serde_json::to_string(&display).unwrap();
        assert_eq!(serde_json::from_str::<String>(&json).unwrap(), display);
    }
}
