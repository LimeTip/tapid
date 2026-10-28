use super::selection::{CommandOwner, SelectedCommand, SelectionError};
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
    selection: SelectedCommand,
) -> Result<LiveCommand, SelectionError> {
    crate::filesystem::activation::validate_managed_node_modules(project)
        .map_err(SelectionError::InconsistentInstallation)?;
    crate::filesystem::tree::verification::verify_materialized_layout(
        project,
        workspace_root,
        plan,
        &input.instances,
        links,
    )
    .map_err(SelectionError::InconsistentInstallation)?;
    if let CommandOwner::Workspace(link) = &selection.owner {
        if links
            .links
            .iter()
            .filter(|candidate| *candidate == link)
            .count()
            != 1
        {
            return Err(SelectionError::InconsistentInstallation(
                "selected workspace placement differs".into(),
            ));
        }
        let source = std::fs::canonicalize(&link.source)
            .map_err(|e| SelectionError::InconsistentInstallation(e.to_string()))?;
        if source != link.source || !source.starts_with(workspace_root) || source == workspace_root
        {
            return Err(SelectionError::InconsistentInstallation(
                "workspace owner escapes project".into(),
            ));
        }
        let entry = source.join(&selection.relative_entry);
        super::selection::validate_selected_entry(&selection.owner, &entry)?;
        return Ok(LiveCommand { selection, entry });
    }
    let CommandOwner::Authenticated(owner) = &selection.owner else {
        unreachable!()
    };
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
        .find(|instance| tapid_linker::InstanceKey::from(*instance) == *owner)
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
        || entry.instance != owner.id
        || entry.peer_context != owner.peer_context.to_string()
        || entry.platform_context != owner.platform_context.to_string()
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
