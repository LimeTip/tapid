use super::{diagnostic, envelope, recorded_text};
use crate::application::info::Collection;
use crate::application::{
    info::InfoReport,
    outcome::{ErrorKind, OperationalError},
};
use serde_json::{Value, json};
use std::process::ExitCode;
use tapid_registry_client::NpmTagTarget;

pub(crate) fn info(report: Result<&InfoReport, &OperationalError>) -> (Value, ExitCode) {
    let mut versions = report.as_ref().map_or(0, |report| report.pages.size);
    let mut tags = versions;
    loop {
        let (mut value, status) = render(report, versions, tags);
        let trimmed =
            report.is_ok_and(|report| versions < report.pages.size || tags < report.pages.size);
        if report.is_ok() {
            value["data"]["output_budget_limited"] = json!(trimmed);
        }
        if trimmed && value["outcome"] == "success" {
            value["outcome"] = json!("partial");
        }
        // emit writes compact JSON followed by one newline. Measure the complete
        // envelope, including diagnostics, cursor records and truncation pointers.
        if serde_json::to_vec(&value).expect("info envelope").len() < 4 * 1024 * 1024 {
            return (value, status);
        }
        if versions > 1 {
            versions = versions.div_ceil(2);
        } else if tags > 1 {
            tags = tags.div_ceil(2);
        } else {
            let error = OperationalError::new(
                ErrorKind::RegistryResultLimit,
                "fixed registry report exceeds output budget",
            );
            return render(Err(&error), 0, 0);
        }
    }
}

fn render(
    report: Result<&InfoReport, &OperationalError>,
    version_limit: usize,
    tag_limit: usize,
) -> (Value, ExitCode) {
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            let mut value = envelope("info", "failure", None);
            diagnostic::record(&mut value, error, "operation");
            return (value, ExitCode::from(1));
        }
    };
    let selected = report.facts.is_some();
    let mut value = envelope("info", if selected { "success" } else { "failure" }, None);
    let mut truncated = Vec::new();
    let versions = report
        .inventory
        .versions()
        .iter()
        .skip(report.pages.versions_start)
        .take(version_limit)
        .enumerate()
        .map(|(index, version)| {
            json!(recorded_text(
                version,
                &format!("/data/versions/entries/{index}"),
                &mut truncated
            ))
        })
        .collect::<Vec<_>>();
    let tags = report.inventory.tags().iter().skip(report.pages.tags_start).take(tag_limit).enumerate().map(|(index, (name, target))| {
        let (status, version) = match target {
            NpmTagTarget::Version(version) => ("reported", Some(version)),
            NpmTagTarget::MissingVersion(version) => ("missing_version", Some(version)),
            NpmTagTarget::Unsupported => ("unsupported", None),
        };
        json!({"name":recorded_text(name, &format!("/data/tags/entries/{index}/name"), &mut truncated), "status":status, "target":version.map(|v| recorded_text(v, &format!("/data/tags/entries/{index}/target"), &mut truncated))})
    }).collect::<Vec<_>>();
    value["data"] = json!({
        "source": {"kind":"npm", "origin": recorded_text(&report.inventory.origin, "/data/source/origin", &mut truncated), "mode": if report.snapshot { "snapshot" } else { "network" }, "freshness":"unknown"},
        "package":recorded_text(&report.inventory.name, "/data/package", &mut truncated),
        "selection":{"requested":recorded_text(&report.selector, "/data/selection/requested", &mut truncated), "kind":report.selector_kind, "status":report.selection_status, "version":report.facts.as_ref().map(|facts| recorded_text(&facts.identity.version, "/data/selection/version", &mut truncated))},
        "versions":page(report, Collection::Versions, versions),
        "tags":page(report, Collection::Tags, tags),
        "checks":{"identity":"checked", "project":"not_checked", "peers":"not_checked", "artifact_integrity":"not_checked", "safety":"not_checked"}
    });
    value["data"]["facts"] = declarations(report, &mut truncated);
    let displayed_node = report
        .facts
        .as_ref()
        .and_then(|facts| facts.engines.as_ref())
        .and_then(|engines| engines.keys().take(100).position(|name| name == "node"));
    let incomplete_node = displayed_node.is_none_or(|index| {
        truncated
            .iter()
            .any(|pointer| pointer == &format!("/data/facts/engines/entries/{index}/requirement"))
    });
    let node_status = if report.node_version.is_none() {
        "not_checked"
    } else if report.node_match.is_none() || incomplete_node {
        "unsupported"
    } else {
        "checked"
    };
    value["data"]["checks"]["engine_requirements"] = json!(report.facts.as_ref().and_then(|facts| facts.engines.as_ref()).into_iter().flat_map(|engines| engines.keys()).take(100).enumerate().map(|(index, name)| json!({"fact_index":index, "status":if name == "node" { node_status } else { "unsupported" }})).collect::<Vec<_>>());
    value["data"]["checks"]["peer_requirements"] = json!(report.facts.as_ref().and_then(|facts| facts.peers.as_ref()).into_iter().flatten().take(100).enumerate().map(|(index, peer)| json!({"fact_index":index, "status":if report.peer_requirement_support.get(&peer.name) == Some(&true) { "supported" } else { "unsupported" }})).collect::<Vec<_>>());
    value["data"]["checks"]["node"] = json!({"status":node_status, "reason":if node_status != "unsupported" { None } else if displayed_node.is_none() { Some("missing_or_omitted_requirement") } else if incomplete_node { Some("incomplete_display") } else { Some("unsupported_expression") }, "supplied":report.node_version.as_ref().map(|version| recorded_text(version, "/data/checks/node/supplied", &mut truncated)), "outcome": if node_status == "checked" { Some(if report.node_match == Some(true) { "match" } else { "mismatch" }) } else { None }, "scope":"reported_node_requirement_only"});
    let omitted = ["peers", "engines", "executables"].iter().any(|name| {
        value["data"]["facts"][name]["known_omitted"]
            .as_u64()
            .is_some_and(|n| n > 0)
    });
    let unknown_engines = report
        .facts
        .as_ref()
        .and_then(|facts| facts.engines.as_ref())
        .is_some_and(|engines| engines.keys().any(|name| name != "node"));
    let unsupported_peers = report
        .peer_requirement_support
        .values()
        .any(|supported| !supported);
    if selected
        && (!truncated.is_empty()
            || omitted
            || unknown_engines
            || unsupported_peers
            || report.inventory.unsupported_versions() > 0
            || value["data"]["tags"]["unsupported"]
                .as_u64()
                .is_some_and(|count| count > 0)
            || node_status == "unsupported")
    {
        value["outcome"] = json!("partial");
    }
    value["data"]["limitations"] = json!([
        "Registry declarations are unverified. No project or peer resolution, artifact verification, lifecycle inspection, or package safety assessment is performed.",
        "Unknown engine names are not evaluated. Missing deprecation does not imply safety."
    ]);
    value["truncated_fields"] = json!(truncated);
    if !selected {
        use crate::application::outcome::{Declaration, DiagnosticContext, DiagnosticPackage};
        let context = DiagnosticContext {
            package: Some(DiagnosticPackage {
                source: report.inventory.origin.to_string(),
                name: report.inventory.name.to_string(),
                version: None,
            }),
            declaration: (report.selector_kind == "tag")
                .then(|| Declaration::metadata("dist-tags", &report.selector)),
            ..DiagnosticContext::default()
        };
        diagnostic::record(
            &mut value,
            &OperationalError::new(
                ErrorKind::RegistrySelection,
                "no supported selection within reported coverage",
            )
            .with_diagnostic(context),
            "operation",
        );
    }
    (
        value,
        if selected {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        },
    )
}

fn page(report: &InfoReport, kind: Collection, entries: Vec<Value>) -> Value {
    let (start, total, unsupported) = match kind {
        Collection::Versions => (
            report.pages.versions_start,
            report.inventory.versions().len(),
            report.inventory.unsupported_versions(),
        ),
        Collection::Tags => (
            report.pages.tags_start,
            report.inventory.tags().len(),
            report
                .inventory
                .tags()
                .values()
                .filter(|target| matches!(target, NpmTagTarget::Unsupported))
                .count(),
        ),
    };
    let returned = entries.len();
    json!({"status":if kind == Collection::Tags && !report.inventory.tags_reported() { "missing" } else { "reported" }, "entries":entries, "returned":returned, "preceding":start, "total_supported":total, "known_omitted":total-start-returned, "complete":start+returned==total && unsupported==0, "unsupported":unsupported, "next":report.pages.next(kind, returned, total)})
}

fn declarations(report: &InfoReport, truncated: &mut Vec<String>) -> Value {
    let Some(facts) = &report.facts else {
        return Value::Null;
    };
    let peers = facts.peers.as_ref().map(|peers| peers.iter().take(100).enumerate().map(|(i, peer)| json!({
        "name":recorded_text(&peer.name, &format!("/data/facts/peers/entries/{i}/name"), truncated),
        "requirement":recorded_text(&peer.requirement, &format!("/data/facts/peers/entries/{i}/requirement"), truncated),
        "optional":peer.optional,
    })).collect::<Vec<_>>());
    let engines = facts.engines.as_ref().map(|engines| engines.iter().take(100).enumerate().map(|(i, (name, expression))| json!({
        "name":recorded_text(name, &format!("/data/facts/engines/entries/{i}/name"), truncated),
        "requirement":recorded_text(expression, &format!("/data/facts/engines/entries/{i}/requirement"), truncated),
    })).collect::<Vec<_>>());
    let executables = facts.executables.as_ref().map(|names| names.iter().take(100).enumerate().map(|(i, name)| json!({"name":recorded_text(name, &format!("/data/facts/executables/entries/{i}/name"), truncated)})).collect::<Vec<_>>());
    json!({
        "provenance":"registry_reported_unverified",
        "peers":collection(peers, facts.peers.as_ref().map_or(0, Vec::len)),
        "engines":collection(engines, facts.engines.as_ref().map_or(0, std::collections::BTreeMap::len)),
        "executables":collection(executables, facts.executables.as_ref().map_or(0, Vec::len)),
        "deprecated":{"status": if facts.deprecated.is_some() { "reported" } else { "missing" }, "value":facts.deprecated.as_ref().map(|text| recorded_text(text, "/data/facts/deprecated/value", truncated))},
    })
}

fn collection(entries: Option<Vec<Value>>, total: usize) -> Value {
    let returned = entries.as_ref().map_or(0, Vec::len);
    json!({"status":if entries.is_some() { "reported" } else { "missing" }, "entries":entries.unwrap_or_default(), "returned":returned, "known_omitted":total-returned, "complete":total==returned})
}
