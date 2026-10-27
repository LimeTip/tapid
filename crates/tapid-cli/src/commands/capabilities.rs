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

const SCHEMAS: &[(&str, &str)] = &[(
    "tapid.run-preflight.v1",
    include_str!("../../schemas/cli/run-preflight.v1.schema.json"),
)];

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
    println!("JSON Schema coverage: partial; run-preflight v1 is embedded.");
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
        "install" | "replay" | "registry_inspection" | "tool_install" | "upgrade" | "execution"
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
    if path == "run" {
        modes[0]["separate_channels"] = json!(["result-file", "receipt-json"]);
        let mut preflight = mode("preflight", "inspection", "stdout_envelope");
        preflight["when"] = json!({"preflight": true});
        preflight["schema_coverage"] = json!("payload");
        preflight["schema"] = json!("tapid.run-preflight.v1");
        modes.push(preflight);
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
            {"command": "outdated", "argument": "json_limit", "requires_global": "json"},
            {"command": "run", "global_json_requires": "preflight"},
            {"command": "run", "preflight_defers": ["environment_values", "backend_probes", "enforcement", "cleanup"]}
        ],
        "help_and_version": {"channel": "human_text", "json": "stdout_envelope", "effects": "no_project_mutation"},
        "schema_catalog": SCHEMAS.iter().map(|(id, _)| json!({"id": id, "target": "data", "contract": "tapid.run-preflight", "schema_version": 1})).collect::<Vec<_>>(), "schema_coverage": "partial",
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
