use super::*;

pub(crate) fn pack(result: &crate::application::pack::PackResult) -> ExitCode {
    let artifact = &result.artifact;
    let files = artifact.files().iter().map(|file| json!({
        "path": file.path, "size": file.size, "sha256": file.digest.to_string(), "mode": file.mode,
    })).collect::<Vec<_>>();
    completed(
        &result.outcome,
        "pack",
        json!({
            "name": artifact.name(), "version": artifact.version(), "preview": result.preview,
            "destination": result.destination.as_deref().map(paths::encode), "files": files,
            "file_count": artifact.files().len(), "content_bytes": artifact.content_bytes(),
            "archive": {"size": artifact.bytes().len(), "sha256": artifact.digest().to_string()},
            "gitignore_not_used": artifact.gitignore_not_used(), "selection_policy_version": 1, "snapshot": {"consistency": "not_atomic"},
        }),
    )
}
