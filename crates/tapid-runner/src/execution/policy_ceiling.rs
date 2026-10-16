//! Shared configured-policy ceilings for invocation-selected authority.
use super::*;

fn exceeds(dimension: &str) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::PolicyViolation,
        format!("invocation {dimension} permission exceeds the configured run policy"),
    )
}

fn bounded<T: Copy + Ord>(requested: Option<T>, ceiling: Option<T>) -> bool {
    ceiling.is_none_or(|limit| requested.is_some_and(|value| value <= limit))
}

pub(super) fn validate(
    request: &ExecutionRequest,
    root: &Path,
    read: &[ResolvedGrant],
    write: &[ResolvedGrant],
) -> Result<(), ExecutionError> {
    let Some(ceiling) = &request.policy_ceiling else {
        return Ok(());
    };
    let policy = request.policy();
    if ceiling.mode() == SandboxMode::Required && policy.mode() != SandboxMode::Required {
        return Err(exceeds("sandbox mode"));
    }
    if ceiling.assurance() == AssuranceLevel::ManagedTree
        && policy.assurance() != AssuranceLevel::ManagedTree
    {
        return Err(exceeds("assurance"));
    }
    if policy.network() && !ceiling.network() {
        return Err(exceeds("network"));
    }
    if policy.subprocess() && !ceiling.subprocess() {
        return Err(exceeds("subprocess"));
    }
    if policy.environment().iter().any(|name| {
        !ceiling.environment().iter().any(|allowed| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case(allowed)
            } else {
                name == allowed
            }
        })
    }) {
        return Err(exceeds("environment"));
    }
    let limits = policy.limits();
    let cap = ceiling.limits();
    if !bounded(limits.timeout_seconds(), cap.timeout_seconds())
        || !bounded(limits.max_output_bytes(), cap.max_output_bytes())
        || !bounded(limits.max_processes(), cap.max_processes())
        || !bounded(limits.max_memory_bytes(), cap.max_memory_bytes())
    {
        return Err(exceeds("resource limit"));
    }
    for (requested, configured, access, dimension) in [
        (
            read,
            ceiling.filesystem().read(),
            FilesystemAccess::Read,
            "read",
        ),
        (
            write,
            ceiling.filesystem().write(),
            FilesystemAccess::Write,
            "write",
        ),
    ] {
        let allowed = configured
            .iter()
            .map(|path| resolve_project_grant(root, path, access))
            .collect::<Result<Vec<_>, _>>()?;
        for grant in requested {
            if !allowed.iter().any(|cap| {
                grant.path == cap.path
                    || (cap.kind == FilesystemGrantKind::DirectorySubtree
                        && grant.path.starts_with(&cap.path))
            }) {
                return Err(exceeds(dimension));
            }
        }
    }
    Ok(())
}
