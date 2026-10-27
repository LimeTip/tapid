//! Read-only projection of the same script and request planning used by launch.
use super::{Args, planning};
use serde_json::json;
use std::{collections::BTreeMap, process::ExitCode};

pub(super) fn run(args: &Args, machine: bool) -> ExitCode {
    let mut data = json!({
        "contract": "tapid.run-preflight", "schema_version": 1,
        "tapid_version": env!("CARGO_PKG_VERSION"),
        "assessment": "blocked", "enforcement_established": false,
        "script": {"name": args.script, "workspace": args.workspace},
        "policy_inputs": null, "runtime": null, "blockers": [],
        "snapshot": "not_atomic",
        "checks_not_performed": ["environment_values_and_value_dependent_limits", "runtime_identity_at_launch", "backend_prerequisites_and_native_probes", "filesystem_binding_at_launch", "enforcement", "descendant_cleanup", "script_success"],
        "dimensions": (["filesystem", "network", "environment", "subprocess", "timeout", "output", "process_count", "memory", "descendant_cleanup"].map(|dimension| json!({"dimension": dimension, "support": "unchecked", "evidence": "deferred_until_launch"}))),
    });
    let mut preparation_reason = None;
    let mut policy_inputs = None;
    let outcome = planning::select(args, &mut policy_inputs).and_then(|plan| {
        data["script"]["project"] = json!(plan.project_dir);
        data["script"]["working_directory"] = json!(plan.working_directory);
        crate::run::validate_allowlisted_environment_names(plan.selection.policy.environment(), cfg!(windows))
            .map_err(|_| super::RunFailure::EnvironmentInvalid)?;
        // An empty map means values were deliberately not collected. It does
        // not stand in for their eventual validation or establish launchability.
        let prepared = plan.prepare(args, &BTreeMap::new())
            .map_err(|error| {
                preparation_reason = Some(match error {
                    crate::run::RunPreparationError::InvalidNodeRuntime => ("RUNTIME_INVALID", "select_node_runtime", "Select an existing executable named node or node.exe."),
                    crate::run::RunPreparationError::MissingNodeRuntime => ("RUNTIME_MISSING", "select_node_runtime", "Select an existing executable Node runtime or provide an absolute runtime directory on PATH."),
                    crate::run::RunPreparationError::InvalidManagedBin => ("MANAGED_BIN_INVALID", "review_managed_bin", "Inspect node_modules and its .bin directory; symlinks and non-directory entries are rejected."),
                    _ => ("PREPARATION_FAILED", "review_run_inputs", "Review runtime, filesystem grants, permission ceilings and request limits."),
                });
                super::RunFailure::PreparationFailed
            })?;
        data["runtime"] = json!({"path": prepared.node_runtime(), "selection": "locally_validated_path", "identity": "deferred_until_launch"});
        data["script"]["working_directory"] = json!(prepared.request().working_directory());
        let blocker = prepared.request().static_platform_blocker();
        data["platform"] = json!({"os": std::env::consts::OS, "evidence": "static_declaration", "support": if blocker.is_some() { "unsupported" } else { "unchecked" }});
        if let Some(blocker) = blocker {
            preparation_reason = Some((blocker.code(), "select_supported_platform_or_policy", blocker.description()));
            return Err(super::RunFailure::PreparationFailed);
        }
        Ok(())
    });
    if let Some(inputs) = policy_inputs {
        data["policy_inputs"] = inputs;
    }
    if let Err(error) = outcome {
        let (code, action, description) = preparation_reason.unwrap_or((error.code(), "review_run_inputs", "Review script selection, runtime path and requested permissions; launch checks remain deferred."));
        data["blockers"] = json!([{
            "code": code,
            "location": {"project": args.project_dir, "manifest": "package.json", "configuration": "tapid.toml"},
            "action": {"id": action, "description": description}
        }]);
    } else {
        data["assessment"] = json!("no_local_blockers");
    }
    if machine {
        return crate::output::json::inspection_result("run-preflight", &args.project_dir, data);
    }
    // Use exactly the same bounded, sanitized projection in both renderers.
    let (data, truncated) = crate::output::json::inspection_data(data);
    println!(
        "Script preflight: {}",
        data["assessment"].as_str().unwrap_or("blocked")
    );
    println!("{data}");
    if truncated || data["assessment"] == "blocked" {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
