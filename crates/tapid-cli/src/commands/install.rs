use crate::application::outcome::OperationFailure;
use clap::Args as ClapArgs;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Resolve and report dependency changes without project or cache writes. May fetch registry metadata.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Package to add to dependencies, such as react@^19.0.0. Defaults to * without a requirement. Online only.
    #[arg(value_parser = parse_package_argument)]
    pub(crate) package: Option<String>,
    /// Replay tapid.lock without network access. Requires a matching manifest and all verified trees in the store.
    #[arg(long)]
    pub(crate) offline: bool,
    /// Require pinned lockfile selections without re-resolution. Downloads missing artifacts unless --offline is set; imported locks may record verified tree receipts.
    #[arg(long)]
    pub(crate) frozen: bool,
    /// Permit npm metadata without registry-declared integrity. Not allowed with --offline or --frozen.
    #[arg(long)]
    pub(crate) allow_unverified_registry_artifacts: bool,
    /// Run unapproved dependency hooks for this invocation. Containment remains required.
    #[arg(long, conflicts_with_all = ["offline", "frozen", "allow_unverified_registry_artifacts"])]
    pub(crate) allow_unapproved_dependency_scripts: bool,
    /// UNSAFE: run dependency hooks without containment. Does not grant hook approval or create reusable verified build outputs.
    #[arg(long, conflicts_with_all = ["offline", "frozen", "allow_unverified_registry_artifacts"])]
    pub(crate) unsafe_no_dependency_sandbox: bool,
    /// Exact project root; omission discovers ancestors and infers the containing member.
    #[arg(long)]
    pub(crate) project_dir: Option<PathBuf>,
    /// Select a workspace member while installing the root workspace graph.
    #[arg(long, value_name = "NAME")]
    pub(crate) workspace: Option<String>,
    /// Verified package store directory. Defaults to tapid/store in the platform cache directory.
    #[arg(long)]
    pub(crate) store_dir: Option<PathBuf>,
    /// Local JSON registry fixture used by tests and air-gapped development.
    #[arg(long)]
    pub(crate) registry_fixture: Option<PathBuf>,
}

/// Rejects bare command words before project access while preserving explicit package specs.
fn parse_package_argument(value: &str) -> Result<String, String> {
    match value.trim() {
        "help" => Err(
            "'help' is ambiguous here. Use 'tapid install --help' or 'tapid help install' for usage; use an explicit package spec such as 'help@1.0.0' to install that package."
                .into(),
        ),
        "install" => Err(
            "'install' is ambiguous here. Use 'tapid install' to install project dependencies; use an explicit package spec such as 'install@1.0.0' to install that package."
                .into(),
        ),
        _ => Ok(value.into()),
    }
}

/// Runs installation or lockfile replay and reports progress, warnings, and the outcome.
pub(crate) fn run(args: Args, json: bool) -> ExitCode {
    if args.dry_run {
        return preview(&args, json);
    }
    let mut reporter = crate::output::install::Reporter::new(json);
    let overrides = crate::application::ExecutionOverrides {
        allow_unapproved: args.allow_unapproved_dependency_scripts,
        without_containment: args.unsafe_no_dependency_sandbox,
    };
    reporter.warnings_before_execution(overrides.warnings());
    if !json && args.allow_unverified_registry_artifacts && !args.offline && !args.frozen {
        eprintln!(
            "warning: npm artifacts without registry integrity are not authenticated against a registry-declared digest"
        );
    }
    let selection = match crate::application::lifecycle::resolve_invocation(
        args.project_dir.as_deref(),
        args.workspace.as_deref(),
    ) {
        Ok(selection) => selection,
        Err(error) => {
            let mut failure = OperationFailure::unchanged(
                args.project_dir
                    .as_deref()
                    .unwrap_or(std::path::Path::new(".")),
                error,
            );
            failure.outcome.warnings.extend(overrides.warnings());
            if json {
                return crate::output::json::failure_or_human(&failure, "install", true);
            }
            reporter.failure(failure);
            return ExitCode::from(1);
        }
    };
    let target_manifest_path = selection.manifest_path;
    let mode = if args.offline {
        crate::application::install::InstallMode::Offline
    } else if args.frozen {
        crate::application::install::InstallMode::Frozen
    } else {
        crate::application::install::InstallMode::Online
    };
    let result = crate::application::install::run_with_execution(
        crate::application::install::InstallRequest {
            project_dir: &selection.root_dir,
            target_manifest_path: &target_manifest_path,
            mutation: args.package.as_deref().map_or(
                crate::application::install::ManifestMutation::None,
                crate::application::install::ManifestMutation::AddPackage,
            ),
            store_root: args.store_dir.as_deref(),
            mode,
            registry_fixture: args.registry_fixture.as_deref(),
            allow_unverified_registry_artifacts: args.allow_unverified_registry_artifacts,
        },
        overrides,
        |event| reporter.progress(event),
    );
    match result {
        Ok(report) => {
            if json {
                return crate::output::json::installed(&report, "install");
            }
            reporter.summary(&report);
            ExitCode::SUCCESS
        }
        Err(error) if json => crate::output::json::failure_or_human(&error, "install", true),
        Err(error) => {
            reporter.failure(error);
            ExitCode::from(1)
        }
    }
}

fn preview(args: &Args, json: bool) -> ExitCode {
    let result = (|| {
        let selection = crate::application::lifecycle::resolve_invocation(
            args.project_dir.as_deref(),
            args.workspace.as_deref(),
        )
        .map_err(|e| {
            OperationFailure::unchanged(
                args.project_dir
                    .as_deref()
                    .unwrap_or(std::path::Path::new(".")),
                e,
            )
        })?;
        if args.package.is_some() && (args.offline || args.frozen) {
            return Err(OperationFailure::unchanged(
                &selection.root_dir,
                crate::application::outcome::OperationalError::new(
                    crate::application::outcome::ErrorKind::InvalidRequest,
                    "a package argument cannot be used with --offline or --frozen",
                ),
            ));
        }
        let next = if let Some(spec) = &args.package {
            let (name, requirement) = crate::package_spec::parse(spec);
            crate::application::lifecycle::plan_add(
                &selection.manifest,
                &[crate::application::lifecycle::DependencyMutation {
                    name: name.to_owned(),
                    requirement: Some(requirement.to_owned()),
                    kind: tapid_manifest::DependencyKind::Dependencies,
                }],
            )
            .map_err(|e| OperationFailure::unchanged(&selection.root_dir, e))?
            .manifest
        } else {
            selection.manifest.clone()
        };
        crate::application::preview::run(
            &selection.root_dir,
            &selection.manifest_path,
            &selection.manifest,
            &next,
            if args.offline {
                crate::application::install::InstallMode::Offline
            } else if args.frozen {
                crate::application::install::InstallMode::Frozen
            } else {
                crate::application::install::InstallMode::Online
            },
            args.registry_fixture.as_deref(),
            args.allow_unverified_registry_artifacts,
        )
    })();
    match result {
        Ok(report) => crate::output::preview::report(&report, "install", json),
        Err(error) => crate::output::json::failure_or_human(&error, "install", json),
    }
}
