use tapid_policy::{AuditOutcome, AuditThreshold, evaluate_audit};
use tapid_protocol::{ReportClassification, ReportState, SecurityFindingV2};

fn advisory(severity: Option<&str>) -> SecurityFindingV2 {
    SecurityFindingV2 {
        id: "REPORT-1".into(),
        revision: "2".into(),
        classification: ReportClassification::Advisory,
        state: ReportState::Active,
        summary: "Known report".into(),
        severity: severity.map(str::to_owned),
        fixes: vec![],
    }
}

#[test]
fn audit_defaults_fail_for_active_reports_and_incomplete_takes_precedence() {
    let report = advisory(Some("low"));
    assert_eq!(
        evaluate_audit([&report], true, AuditThreshold::default()),
        AuditOutcome::Findings
    );
    assert_eq!(
        evaluate_audit([&report], false, AuditThreshold::default()),
        AuditOutcome::Incomplete
    );
    assert_eq!(
        evaluate_audit([], true, AuditThreshold::default()),
        AuditOutcome::BelowThreshold
    );
    assert_eq!(
        evaluate_audit([], false, AuditThreshold::default()),
        AuditOutcome::Incomplete
    );
}

#[test]
fn severity_floor_never_hides_malware_or_unknown_severity_and_ignores_withdrawals() {
    let low = advisory(Some("low"));
    let high = advisory(Some("high"));
    assert_eq!(
        evaluate_audit([&low], true, AuditThreshold::High),
        AuditOutcome::BelowThreshold
    );
    assert_eq!(
        evaluate_audit([&high], true, AuditThreshold::High),
        AuditOutcome::Findings
    );
    for severity in [
        None,
        Some("unknown"),
        Some("HIGH"),
        Some("critical"),
        Some("new-level"),
    ] {
        let report = advisory(severity);
        assert_eq!(
            evaluate_audit([&report], true, AuditThreshold::High),
            AuditOutcome::Findings
        );
    }
    let mut malware = advisory(Some("low"));
    malware.classification = ReportClassification::ReportedMalware;
    assert_eq!(
        evaluate_audit([&malware], true, AuditThreshold::Critical),
        AuditOutcome::Findings
    );
    malware.state = ReportState::Withdrawn;
    assert_eq!(
        evaluate_audit([&malware], true, AuditThreshold::Any),
        AuditOutcome::BelowThreshold
    );
    assert_eq!(
        evaluate_audit([&malware], false, AuditThreshold::Any),
        AuditOutcome::Incomplete
    );
}

#[test]
fn advisory_threshold_boundaries_are_inclusive() {
    let floors = [
        AuditThreshold::Any,
        AuditThreshold::Low,
        AuditThreshold::Moderate,
        AuditThreshold::High,
        AuditThreshold::Critical,
    ];
    let expected = [
        [true, true, true, true],
        [true, true, true, true],
        [false, true, true, true],
        [false, false, true, true],
        [false, false, false, true],
    ];
    for (floor, row) in floors.into_iter().zip(expected) {
        for (severity, fails) in ["low", "moderate", "high", "critical"].into_iter().zip(row) {
            let report = advisory(Some(severity));
            assert_eq!(
                evaluate_audit([&report], true, floor),
                if fails {
                    AuditOutcome::Findings
                } else {
                    AuditOutcome::BelowThreshold
                },
                "{floor:?} / {severity}"
            );
        }
    }
}

#[test]
fn findings_below_threshold_do_not_mask_later_active_reports() {
    let low = advisory(Some("low"));
    let high = advisory(Some("high"));
    let mut withdrawn = advisory(Some("critical"));
    withdrawn.state = ReportState::Withdrawn;
    assert_eq!(
        evaluate_audit([&low, &withdrawn, &high], true, AuditThreshold::High),
        AuditOutcome::Findings
    );
}

#[test]
fn audit_outcomes_have_stable_exit_codes() {
    assert_eq!(AuditOutcome::BelowThreshold.exit_code(), 0);
    assert_eq!(AuditOutcome::Findings.exit_code(), 1);
    assert_eq!(AuditOutcome::Incomplete.exit_code(), 3);
}
