//! Offline installed-binary discovery. Structural facts come from Clap; semantic
//! effects remain explicit because flags alone cannot describe application work.
use clap::CommandFactory;
use serde_json::{Value, json};
use std::process::ExitCode;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// Export one advertised, embedded JSON Schema by identifier (never a path or URL).
    #[arg(long, value_name = "ID")]
    schema: Option<String>,
}

const SCHEMAS: &[(&str, &str)] = &[
    (
        "tapid.dependency-preview.v1",
        include_str!("../../schemas/cli/dependency-preview.v1.schema.json"),
    ),
    (
        "tapid.package-manifest.v1",
        include_str!("../../schemas/cli/package-manifest.v1.schema.json"),
    ),
    (
        "tapid.manifest-validation.v1",
        include_str!("../../schemas/cli/manifest-validation.v1.schema.json"),
    ),
    (
        "tapid.envelope.v1",
        include_str!("../../schemas/cli/envelope.v1.schema.json"),
    ),
    (
        "tapid.run-receipt.v1",
        include_str!("../../schemas/cli/run-receipt.v1.schema.json"),
    ),
    (
        "tapid.run-result.v1",
        include_str!("../../schemas/cli/run-result.v1.schema.json"),
    ),
    (
        "tapid.schema-export.v1",
        include_str!("../../schemas/cli/schema-export.v1.schema.json"),
    ),
    (
        "tapid.status.v1",
        include_str!("../../schemas/cli/status.v1.schema.json"),
    ),
    (
        "tapid.run-preflight.v1",
        include_str!("../../schemas/cli/run-preflight.v1.schema.json"),
    ),
    (
        "tapid.capabilities.v1",
        include_str!("../../schemas/cli/capabilities.v1.schema.json"),
    ),
];

// Every public path, including parents with no executable default, is reviewed.
const COVERAGE: &[(&str, &str, &str)] = &[
    ("", "help", "unsupported"),
    ("capabilities", "inspection", "stdout_envelope"),
    ("status", "inspection", "stdout_envelope"),
    ("run", "execution", "unsupported"),
    ("install", "install", "stdout_envelope"),
    ("ci", "replay", "stdout_envelope"),
    ("add", "install", "stdout_envelope"),
    ("remove", "install", "stdout_envelope"),
    ("update", "install", "stdout_envelope"),
    ("outdated", "registry_inspection", "stdout_envelope"),
    ("prune", "replay", "stdout_envelope"),
    ("why", "inspection", "stdout_envelope"),
    ("explain", "registry_inspection", "stdout_envelope"),
    ("init", "init", "stdout_envelope"),
    ("import-package-lock", "import", "stdout_envelope"),
    ("manifest", "parent", "stdout_envelope"),
    ("manifest validate", "inspection", "stdout_envelope"),
    ("lock", "parent", "stdout_envelope"),
    ("lock verify", "inspection", "stdout_envelope"),
    ("cache", "cache_inspection", "stdout_envelope"),
    ("cache info", "cache_inspection", "stdout_envelope"),
    ("cache clean", "cache_clean", "stdout_envelope"),
    ("clean", "cache_clean", "stdout_envelope"),
    ("tool", "parent", "stdout_envelope"),
    ("tool install", "tool_install", "stdout_envelope"),
    ("tool update", "tool_install", "stdout_envelope"),
    ("tool remove", "tool_remove", "stdout_envelope"),
    ("tool list", "tool_inspection", "stdout_envelope"),
    ("sandbox", "parent", "unsupported"),
    ("sandbox install", "service", "unsupported"),
    ("sandbox uninstall", "service", "unsupported"),
    ("sandbox status", "inspection", "unsupported"),
    ("license", "help", "unsupported"),
    ("upgrade", "upgrade", "unsupported"),
];

pub(crate) fn run(args: Args, machine: bool) -> ExitCode {
    if let Some(id) = args.schema {
        let Some((id, source)) = SCHEMAS.iter().find(|(known, _)| *known == id) else {
            if machine {
                return crate::output::json::protocol_error("capabilities", "SCHEMA_UNKNOWN", 1);
            }
            eprintln!(
                "error: unknown schema identifier; list capabilities for supported identifiers"
            );
            return ExitCode::from(1);
        };
        let schema: Value = serde_json::from_str(source).expect("embedded schema is valid JSON");
        if machine {
            return crate::output::json::information(
                "capabilities",
                json!({"id": id, "schema": schema}),
            );
        }
        println!("{schema}");
        return ExitCode::SUCCESS;
    }
    let data = discover();
    if machine {
        return crate::output::json::information("capabilities", data);
    }
    println!(
        "Tapid {} capabilities (contract v1)",
        env!("CARGO_PKG_VERSION")
    );
    for command in data["commands"].as_array().expect("command list") {
        let path = command["path"].as_str().unwrap();
        if path.is_empty() || path.split(' ').any(|part| part == "help") {
            continue;
        }
        let channels = command["modes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|mode| {
                format!(
                    "{}={}",
                    mode["id"].as_str().unwrap(),
                    mode["json"].as_str().unwrap()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!("{path}: {channels}");
    }
    println!(
        "Help/version support JSON envelopes. Ordinary run has separate result-file/receipt channels."
    );
    println!(
        "JSON Schema coverage: partial; common envelope, package manifest input, manifest validation, status, discovery, schema export, run-preflight and run result/receipt v1 are embedded."
    );
    println!(
        "Static declarations only; project compatibility and process enforcement are unchecked."
    );
    ExitCode::SUCCESS
}

fn mode(id: &str, family: &str, channel: &str) -> Value {
    let project_write = matches!(
        family,
        "install" | "replay" | "init" | "import" | "execution"
    );
    let store_write = matches!(
        family,
        "install" | "replay" | "tool_install" | "cache_clean"
    );
    let network = matches!(
        family,
        "install"
            | "replay"
            | "registry_inspection"
            | "dependency_preview"
            | "tool_install"
            | "upgrade"
            | "execution"
    );
    json!({
        "id": id, "json": channel, "maturity": "experimental",
        "evidence": "static_declaration", "schema_coverage": "not_yet_published",
        "effects": {
            "project_writes_possible": project_write, "store_writes_possible": store_write,
            "other_writes_possible": matches!(family, "tool_install" | "tool_remove" | "tool_inspection" | "service" | "upgrade"),
            "network_possible": network,
            "child_execution_possible": matches!(family, "execution" | "install" | "replay" | "service"),
            "hook_execution_possible": family == "install",
            "prompts_or_stdin_possible": family == "execution", "automatic_retry_guaranteed": false,
            "conditions": match family {
                "install" => "Project recovery and installation may write. Online requests may access registries; offline replay does not. Approved or explicitly authorized dependency hooks may execute only in supported online modes.",
                "replay" => "Installed layout and recovery may write. ci may fetch pinned artifacts unless offline, using Git subprocesses for pinned Git sources; prune is offline. Hooks do not execute.",
                "execution" => "Child behavior depends on requested permissions and established backend authority. A result file is created before launch when requested. This declaration establishes no enforcement.",
                "cache_clean" => "Default and --dry-run preview only; --yes removes published store entries.",
                "registry_inspection" => "Network depends on command, --offline and registry fixtures; no dependency installation is performed.",
                "dependency_preview" => "Read-only resolution preview; registry metadata may be fetched unless a local fixture is supplied. No recovery, artifact preparation, installation or hooks are performed.",
                "parent" => "A subcommand is required; see the selected subcommand's effects.",
                "upgrade" => "May update release cache and executable destination; --dry-run does not replace the executable.",
                "tool_install" => "May recover and write standalone tool environments, bins and verified store; accesses registry unless using a fixture. Hooks never run.",
                "tool_inspection" => "An existing tool catalog may recover pending publication and retire inactive owned environments; a missing tools root remains unchanged.",
                "tool_remove" => "May recover and remove standalone tool environment and owned bins.",
                "service" => "Administrator service configuration may change; platform-specific setup is required.",
                _ => "No project code execution or registry access is required by this mode. Local file reads depend on the command."
            }
        },
        "retry": "inspect_typed_failure_and_current_state_before_retrying"
    })
}

fn describe(command: &clap::Command, path: &str, commands: &mut Vec<Value>) {
    let metadata = if path.split(' ').any(|part| part == "help") {
        Some(&("", "help", "stdout_envelope"))
    } else {
        COVERAGE.iter().find(|entry| entry.0 == path)
    };
    let mut modes = vec![match metadata {
        Some((_, family, channel)) => mode("default", family, channel),
        None => json!({"id": "default", "json": "unknown", "coverage": "unclassified"}),
    }];
    if matches!(path, "install" | "add" | "remove" | "update") {
        modes[0]["when"] = json!({"dry_run": false});
        let mut preview = mode("dry_run", "dependency_preview", "stdout_envelope");
        preview["when"] = json!({"dry_run": true});
        preview["schema_coverage"] = json!("payload");
        preview["schema"] = json!("tapid.dependency-preview.v1");
        modes.push(preview);
    }
    if matches!(path, "cache clean" | "clean") {
        modes[0]["when"] = json!({"yes": false});
        modes[0]["effects"]["store_writes_possible"] = json!(false);
        let mut removal = mode("remove", "cache_clean", "stdout_envelope");
        removal["when"] = json!({"yes": true});
        modes.push(removal);
    }
    if path == "manifest validate" {
        modes[0]["schema_coverage"] = json!("payload");
        modes[0]["schema"] = json!("tapid.manifest-validation.v1");
        modes[0]["input_schemas"] = json!(["tapid.package-manifest.v1"]);
    }
    if path == "status" {
        modes[0]["schema_coverage"] = json!("payload");
        modes[0]["schema"] = json!("tapid.status.v1");
    }
    if path == "run" {
        modes[0]["separate_channels"] = json!(["result-file", "receipt-json"]);
        modes[0]["when"] =
            json!({"preflight": false, "result_file": "absent", "receipt_json": false});
        let mut result = mode("result_file", "execution", "unsupported");
        result["when"] = json!({"result_file": "present"});
        result["separate_channels"] = json!(["result-file"]);
        result["schema_coverage"] = json!("payload");
        result["schema"] = json!("tapid.run-result.v1");
        result["trusted_channel"] = json!(true);
        let mut receipt = mode("receipt_json", "execution", "unsupported");
        receipt["when"] = json!({"receipt_json": true});
        receipt["separate_channels"] = json!(["receipt-json"]);
        receipt["schema_coverage"] = json!("payload");
        receipt["schema"] = json!("tapid.run-receipt.v1");
        receipt["trusted_channel"] = json!(false);
        let mut preflight = mode("preflight", "inspection", "stdout_envelope");
        preflight["when"] = json!({"preflight": true});
        preflight["schema_coverage"] = json!("payload");
        preflight["schema"] = json!("tapid.run-preflight.v1");
        modes.push(preflight);
        modes.push(result);
        modes.push(receipt);
    }
    if path == "capabilities" {
        modes[0]["schema_coverage"] = json!("payload");
        modes[0]["schema"] = json!("tapid.capabilities.v1");
        modes[0]["when"] = json!({"schema": "absent"});
        let mut export = mode("schema_export", "inspection", "stdout_envelope");
        export["when"] = json!({"schema": "present"});
        export["schema_coverage"] = json!("payload");
        export["schema"] = json!("tapid.schema-export.v1");
        modes.push(export);
    }
    for mode in &mut modes {
        // These rules only narrow possible effects. All matching rules apply;
        // frozen alone still permits retrieval of missing pinned content.
        if matches!(path, "install" | "ci") {
            let mut rules = vec![json!({
                "when": {"offline": true},
                "effects": {"network_possible": false, "hook_execution_possible": false}
            })];
            if path == "install" {
                rules.push(json!({
                    "when": {"frozen": true},
                    "effects": {"hook_execution_possible": false}
                }));
            }
            mode["conditional_effects"] = json!(rules);
        }
        if mode["json"] == "stdout_envelope" {
            mode["envelope_schema"] = json!("tapid.envelope.v1");
            if mode["schema_coverage"] == "not_yet_published" {
                mode["schema_coverage"] = json!("envelope_only");
            }
        }
    }
    let arguments = command.get_arguments().filter(|arg| !arg.is_hide_set()).map(|arg| {
        json!({
            "id": arg.get_id().as_str(), "long": arg.get_long(), "short": arg.get_short(),
            "aliases": arg.get_visible_aliases().unwrap_or_default(),
            "short_aliases": arg.get_visible_short_aliases().unwrap_or_default(),
            "required": arg.is_required_set(), "global": arg.is_global_set(), "position": arg.get_index(),
            "action": format!("{:?}", arg.get_action()),
            "arity": arg.get_num_args().map(|range| json!({"min": range.min_values(), "max": range.max_values()})),
            "defaults": arg.get_default_values().iter().map(|value| value.to_string_lossy()).collect::<Vec<_>>(),
            "conflicts": command.get_arg_conflicts_with(arg).iter().map(|arg| arg.get_id().as_str()).collect::<Vec<_>>(),
            "possible_values": arg.get_value_parser().possible_values().map(|values| values.filter(|value| !value.is_hide_set()).map(|value| value.get_name().to_owned()).collect::<Vec<_>>()),
        })
    }).collect::<Vec<_>>();
    if path == "prune" {
        modes[0]["effects"]["network_possible"] = json!(false);
        modes[0]["effects"]["child_execution_possible"] = json!(false);
    }
    commands.push(json!({
        "path": path, "aliases": command.get_visible_aliases().collect::<Vec<_>>(),
        "arguments": arguments, "modes": modes,
        "subcommand_required": command.is_subcommand_required_set(),
        "groups": command.get_groups().map(|group| json!({"id": group.get_id().as_str(), "arguments": group.get_args().map(|id| id.as_str()).collect::<Vec<_>>(), "required": group.is_required_set()})).collect::<Vec<_>>()
    }));
    for child in command
        .get_subcommands()
        .filter(|child| !child.is_hide_set())
    {
        let child_path = if path.is_empty() {
            child.get_name().to_owned()
        } else {
            format!("{path} {}", child.get_name())
        };
        describe(child, &child_path, commands);
    }
}

fn discover() -> Value {
    let mut command = super::Cli::command();
    command.build();
    let mut commands = Vec::new();
    describe(&command, "", &mut commands);
    json!({
        "contract": "tapid.capabilities", "schema_version": 1,
        "tapid_version": env!("CARGO_PKG_VERSION"), "commands": commands,
        "compatibility": {"schema_version_independent_of_tapid_version": true, "unknown_schema_version": "unsupported", "unknown_fields": "ignore_additive", "unknown_reason_or_identifier": "unchecked", "incomplete_coverage": "unchecked"},
        "constraints": [
            {"command": "install", "argument": "package", "conflicts_with_any": ["offline", "frozen"]},
            {"command": "install", "argument": "allow_unverified_registry_artifacts", "conflicts_with_any": ["offline", "frozen"]},
            {"command": "outdated", "argument": "json_limit", "requires_global": "json"},
            {"command": "run", "global_json_requires": "preflight"},
            {"command": "run", "preflight_defers": ["environment_values", "backend_probes", "enforcement", "cleanup"]}
        ],
        "help_and_version": {"channel": "human_text", "json": "stdout_envelope", "effects": "no_project_mutation"},
        "schema_catalog": SCHEMAS.iter().map(|(id, source)| {
            let schema: Value = serde_json::from_str(source).expect("embedded schema is valid JSON");
            let contract = match *id {
                "tapid.dependency-preview.v1" => json!("tapid.dependency-preview"),
                "tapid.package-manifest.v1" => json!("tapid.package-manifest"),
                "tapid.envelope.v1" => json!("tapid.envelope"),
                "tapid.manifest-validation.v1" => json!("tapid.manifest-validation"),
                "tapid.status.v1" => json!("tapid.status"),
                "tapid.run-result.v1" => json!("tapid.run-result"),
                "tapid.run-receipt.v1" => json!("tapid.run-receipt"),
                "tapid.schema-export.v1" => json!("tapid.schema-export"),
                _ => schema["properties"]["contract"]["const"].clone(),
            };
            json!({"id": id, "target": if *id == "tapid.package-manifest.v1" { "input" } else if *id == "tapid.envelope.v1" { "envelope" } else if *id == "tapid.run-result.v1" { "result_file" } else if *id == "tapid.run-receipt.v1" { "stderr_receipt" } else { "data" }, "contract": contract, "schema_version": 1})
        }).collect::<Vec<_>>(), "schema_coverage": "partial",
        "assurance": {"package_manager_support": "separate_from_runner", "process_enforcement": "not_established", "project_compatibility": "unchecked"}
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn coverage_matches_every_public_clap_path() {
        let data = super::discover();
        let discovered = data["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|command| command["path"].as_str().unwrap())
            .filter(|path| !path.split(' ').any(|part| part == "help"))
            .collect::<std::collections::BTreeSet<_>>();
        let maintained = super::COVERAGE
            .iter()
            .map(|entry| entry.0)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(discovered, maintained);
        assert_eq!(maintained.len(), super::COVERAGE.len());
    }
}
