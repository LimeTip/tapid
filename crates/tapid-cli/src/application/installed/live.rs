use super::selection::{SelectedCommand, SelectionError, select_command};
use std::path::{Path, PathBuf};
use tapid_linker::{MaterializationPlan, NamedLayoutInput, WorkspaceLinkPlan};

/// Observed live placement. This does not retain project/store coordination or
/// confer launch authority; callers must retain guards and revalidate at launch.
#[derive(Debug)]
#[allow(dead_code)] // Consumed by project execution composition once integrated.
pub(super) struct LiveCommand {
    pub selection: SelectedCommand,
    pub entry: PathBuf,
}

/// Bind reference selection to one exact, completely verified live layout.
#[allow(dead_code)] // Project snapshot composition is not yet exposed by the CLI.
pub(super) fn bind_live_command(
    project: &Path,
    workspace_root: &Path,
    input: &NamedLayoutInput,
    plan: &MaterializationPlan,
    links: &WorkspaceLinkPlan,
    package: Option<&str>,
    command: &str,
) -> Result<LiveCommand, SelectionError> {
    crate::filesystem::activation::validate_managed_node_modules(project)
        .map_err(SelectionError::InconsistentInstallation)?;
    let selection = select_command(
        input,
        package,
        command,
        crate::application::replay::current_platform(),
    )?;
    crate::filesystem::tree::verification::verify_materialized_layout(
        project,
        workspace_root,
        plan,
        &input.instances,
        links,
    )
    .map_err(SelectionError::InconsistentInstallation)?;
    let target = project.join("node_modules").join(
        selection
            .install_name
            .as_str()
            .split('/')
            .collect::<PathBuf>(),
    );
    let mut placements = plan
        .activation
        .steps
        .iter()
        .filter(|step| step.target == target);
    let placement = placements.next().ok_or_else(|| {
        SelectionError::InconsistentInstallation("selected binding has no live placement".into())
    })?;
    if placements.next().is_some() {
        return Err(SelectionError::InconsistentInstallation(
            "duplicate live placement".into(),
        ));
    }
    let instance = input
        .instances
        .iter()
        .find(|instance| tapid_linker::InstanceKey::from(*instance) == selection.owner)
        .ok_or_else(|| {
            SelectionError::InconsistentInstallation("selected owner is missing".into())
        })?;
    let mut entries = plan
        .entries
        .iter()
        .filter(|entry| entry.target == placement.source);
    let entry = entries.next().ok_or_else(|| {
        SelectionError::InconsistentInstallation("selected placement source is missing".into())
    })?;
    if entries.next().is_some()
        || entry.instance != selection.owner.id
        || entry.peer_context != selection.owner.peer_context.to_string()
        || entry.platform_context != selection.owner.platform_context.to_string()
        || entry.source != instance.tree.root
    {
        return Err(SelectionError::InconsistentInstallation(
            "selected placement owner differs".into(),
        ));
    }
    let entry = target.join(&selection.relative_entry);
    tapid_linker::validate_node_entry(&entry).map_err(SelectionError::InvalidMapping)?;
    Ok(LiveCommand { selection, entry })
}
