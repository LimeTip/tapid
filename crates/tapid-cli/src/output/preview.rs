use crate::application::preview::PreviewReport;
use std::process::ExitCode;

pub(crate) fn report(report: &PreviewReport, operation: &str, json: bool) -> ExitCode {
    if json {
        return super::json::preview(report, operation);
    }
    super::report_warnings(&report.outcome.warnings);
    println!("Dry run: resolved dependency graph. Artifacts are not prepared.");
    let data = serde_json::to_value(report).expect("preview serialization");
    let value = |value: &serde_json::Value| value.as_str().unwrap_or("absent").to_owned();
    println!("Declaration changes:");
    for declaration in data["declarations"].as_array().expect("declarations") {
        println!(
            "  {}",
            super::json::text(format!(
                "{} [{}]: {} -> {}",
                value(&declaration["name"]),
                value(&declaration["section"]),
                value(&declaration["before"]),
                value(&declaration["after"])
            ))
            .0
        );
    }
    if report.declarations.is_empty() {
        println!("  None.");
    }
    println!("Resolved graph changes:");
    let mut count = 0;
    for root in data["root_changes"].as_array().expect("root changes") {
        count += 1;
        println!(
            "  {}",
            super::json::text(format!(
                "root {}: {} -> {}",
                value(&root["name"]),
                value(&root["before"]),
                value(&root["after"])
            ))
            .0
        );
    }
    for package in data["packages"].as_array().expect("packages") {
        if package["change"] == "unchanged" {
            continue;
        }
        count += 1;
        println!(
            "  {}",
            super::json::text(format!(
                "{} {}",
                value(&package["change"]),
                value(&package["key"])
            ))
            .0
        );
        for reason in package["reasons"].as_array().expect("reasons") {
            let detail = if reason["relation"] == "direct" {
                format!(
                    "{}: direct dependency {}",
                    value(&reason["graph"]),
                    value(&reason["dependency"])
                )
            } else {
                format!(
                    "{}: {} requires {}{}",
                    value(&reason["graph"]),
                    value(&reason["parent"]),
                    value(&reason["dependency"]),
                    reason["requirement"]
                        .as_str()
                        .map(|r| format!("@{r}"))
                        .unwrap_or_default()
                )
            };
            println!("    {}", super::json::text(detail).0);
        }
        if package["change"] == "changed" {
            println!(
                "    Integrity metadata or dependency targets changed; inspect --json for before/after records."
            );
        }
    }
    if count == 0 {
        println!("  None.");
    }
    let effects = &data["effects"];
    println!(
        "Effects: network {}; metadata source {}; {} package metadata lookup(s).",
        if effects["network_allowed"] == true {
            "allowed for registry metadata"
        } else {
            "disabled"
        },
        value(&effects["metadata_source"]),
        effects["metadata_lookups"]
    );
    println!("No artifact requests, cache reads/writes, project writes, recovery, or scripts.");
    for limitation in data["limitations"].as_array().expect("limitations") {
        println!("{}", super::json::text(value(limitation)).0);
    }
    ExitCode::SUCCESS
}
