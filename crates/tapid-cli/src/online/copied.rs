//! Copied artifact declarations, immutable pins, and archive metadata preparation.
use super::*;
use tapid_registry_client::GitDependency;

#[derive(Clone, Debug)]
pub(crate) enum CopiedDeclaration {
    File(String),
    Git(GitDependency),
}

pub(crate) fn declaration(raw: &str) -> Result<Option<CopiedDeclaration>, String> {
    if let Some(path) = raw.strip_prefix("file:") {
        let path = path.strip_prefix("./").unwrap_or(path);
        if path.is_empty()
            || path.starts_with('/')
            || path.contains(['\\', ':', '|', '#'])
            || path.chars().any(char::is_control)
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || ![".tgz", ".tar.gz", ".tar"]
                .iter()
                .any(|extension| path.ends_with(extension))
        {
            return Err("file dependency must name a project-relative .tgz, .tar.gz, or .tar regular file; directory links and traversal are unsupported".into());
        }
        return Ok(Some(CopiedDeclaration::File(path.into())));
    }
    if raw.starts_with("git+") {
        return GitDependency::parse(raw)
            .map(CopiedDeclaration::Git)
            .map(Some);
    }
    if raw.contains("://")
        || raw.starts_with("github:")
        || raw.starts_with("git:")
        || raw.starts_with("ssh:")
    {
        return Err("unsupported dependency protocol; only git+https repositories and file tarballs are supported".into());
    }
    Ok(None)
}

fn declarations(manifest: &PackageManifest) -> impl Iterator<Item = (&String, &String)> {
    manifest
        .dependencies()
        .iter()
        .chain(manifest.dev_dependencies())
        .chain(manifest.optional_dependencies())
}

pub(super) fn validate_declarations(
    project: &Path,
    manifest: &PackageManifest,
) -> Result<(), String> {
    let mut identities = BTreeMap::new();
    for (name, raw) in declarations(manifest) {
        let parsed = declaration(raw).map_err(|reason| format!("dependency '{name}': {reason}"))?;
        let copied = parsed.is_some();
        let mut identity = raw.clone();
        if let Some(parsed) = parsed {
            name.parse::<PackageName>().map_err(|_| {
                format!("copied dependency '{name}' must use a local npm package name")
            })?;
            identity = match parsed {
                CopiedDeclaration::File(path) => {
                    file_bytes(project, &path)
                        .map_err(|reason| format!("dependency '{name}': {reason}"))?;
                    format!("file:{path}")
                }
                CopiedDeclaration::Git(spec) => {
                    format!("git+{}#{}", spec.repository(), spec.reference())
                }
            };
        }
        if identities
            .insert(name, (copied, identity.clone()))
            .is_some_and(|(was_copied, old)| (copied || was_copied) && old != identity)
        {
            return Err(format!(
                "conflicting copied dependency declarations for '{name}'"
            ));
        }
    }
    Ok(())
}

pub(super) fn file_bytes(project: &Path, relative: &str) -> Result<Vec<u8>, String> {
    let root =
        fs::canonicalize(project).map_err(|_| "cannot resolve project for file dependency")?;
    let mut path = root.clone();
    for component in relative.split('/') {
        path.push(component);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| "file dependency is missing or inaccessible")?;
        if metadata.file_type().is_symlink() {
            return Err("file dependency cannot traverse a symlink".into());
        }
    }
    let canonical = fs::canonicalize(&path).map_err(|_| "cannot resolve file dependency")?;
    if !canonical.starts_with(&root) {
        return Err("file dependency escapes the project".into());
    }
    crate::commands::run::read_bounded_config_file(
        &canonical,
        crate::filesystem::atomic::MAX_ARTIFACT_BYTES,
    )
    .map_err(|_| "file dependency must be a regular file no larger than 512 MiB".into())
}

fn scratch() -> Result<TemporaryTree, String> {
    let path = std::env::temp_dir().join(format!(
        "tapid-copied-{}-{}",
        std::process::id(),
        crate::filesystem::atomic::unique_nonce()
    ));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&path)
        .map_err(|_| "cannot create private copied-artifact staging directory")?;
    Ok(TemporaryTree(path))
}

pub(super) fn archive_format(source: &PackageSource, bytes: &[u8]) -> ArchiveFormat {
    if source.registry().is_none() && !bytes.starts_with(&[0x1f, 0x8b]) {
        ArchiveFormat::Tar
    } else {
        ArchiveFormat::TarGz
    }
}

fn record(source: PackageSource, bytes: Vec<u8>) -> Result<PackageRecord, OperationalError> {
    if source.artifact_digest() != Some(digest(&bytes).as_str()) {
        return Err(OperationalError::new(
            ErrorKind::Integrity,
            "copied artifact digest mismatch",
        ));
    }
    let staging = scratch()?;
    let tree = staging.0.join("tree");
    extract_to(
        &bytes,
        archive_format(&source, &bytes),
        &tree,
        ArchiveLimits::default(),
    )
    .map_err(|error| OperationalError::from_source(ErrorKind::Archive, error))?;
    let root = crate::filesystem::tree::package_content_root(&tree)?;
    let manifest_bytes =
        crate::commands::run::read_bounded_config_file(&root.join("package.json"), 1024 * 1024)
            .map_err(|_| "copied package requires a regular package.json no larger than 1 MiB")?;
    let text =
        std::str::from_utf8(&manifest_bytes).map_err(|_| "copied package.json is not UTF-8")?;
    let manifest = PackageManifest::parse(text)
        .map_err(|error| OperationalError::from_source(ErrorKind::Manifest, error))?;
    let platform_json: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "invalid copied package.json")?;
    let platform_list = |field: &str| -> Result<Vec<String>, String> {
        let Some(value) = platform_json.get(field) else {
            return Ok(Vec::new());
        };
        let values = value
            .as_array()
            .ok_or_else(|| format!("copied package {field} must be an array"))?;
        values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| format!("copied package {field} must contain nonempty strings"))
            })
            .collect()
    };
    let peer_meta = platform_json
        .get("peerDependenciesMeta")
        .and_then(serde_json::Value::as_object);
    let optional_peer_dependencies = peer_meta
        .into_iter()
        .flat_map(|meta| meta.iter())
        .filter(|(_, value)| {
            value.get("optional").and_then(serde_json::Value::as_bool) == Some(true)
        })
        .map(|(name, _)| name.clone())
        .collect();
    Ok(PackageRecord {
        git_reference: None,
        registry: source,
        name: manifest.name().clone(),
        version: manifest.version().clone(),
        dist_tags: BTreeSet::new(),
        integrity: Some(integrity(&bytes)),
        artifact: String::new(),
        dependencies: manifest.dependencies().clone(),
        peer_dependencies: manifest.peer_dependencies().clone(),
        optional_peer_dependencies,
        optional_dependencies: manifest.optional_dependencies().clone(),
        platform: PackagePlatform {
            os: platform_list("os")?,
            cpu: platform_list("cpu")?,
            libc: platform_list("libc")?,
        },
        fixture: false,
        copied_archive: Some(std::sync::Arc::new(bytes)),
    })
}

pub(super) fn prepare_roots(
    project: &Path,
    manifest: &PackageManifest,
    previous: Option<&Lockfile>,
    fixture_records: &BTreeMap<PackageRecordKey, PackageRecord>,
    using_fixture: bool,
) -> Result<Vec<(Dependency, PackageRecord)>, OperationalError> {
    let current_digest = root_digest(project)?;
    let previous = previous.filter(|lock| lock.root_manifest_digest() == current_digest);
    let mut prepared = BTreeMap::<PackageName, (Dependency, PackageRecord)>::new();
    for (local, raw) in declarations(manifest) {
        let Some(spec) =
            declaration(raw).map_err(|reason| format!("dependency '{local}': {reason}"))?
        else {
            continue;
        };
        let local_name: PackageName = local
            .parse()
            .map_err(|_| "invalid copied dependency name")?;
        if prepared.contains_key(&local_name) {
            continue;
        }
        let pinned = previous
            .and_then(|lock| lock.root_bindings().get(local))
            .and_then(|key| key.parse::<LockfilePackageKey>().ok())
            .and_then(|key| key.source.package_source());
        let package = match spec {
            CopiedDeclaration::File(path) => {
                let bytes = file_bytes(project, &path)?;
                let source = PackageSource::file(&path, digest(&bytes))
                    .map_err(|error| error.to_string())?;
                record(source, bytes)?
            }
            CopiedDeclaration::Git(spec) => {
                let pinned = pinned.filter(|source| matches_source(raw, source).unwrap_or(false));
                let fixture_record = fixture_records.values().find(|record| {
                    record
                        .registry
                        .git_parts()
                        .is_some_and(|(repository, commit, _)| {
                            repository == spec.repository().as_str()
                                && pinned.as_ref().map_or_else(
                                    || {
                                        if is_commit(spec.reference()) {
                                            spec.reference().eq_ignore_ascii_case(commit)
                                        } else {
                                            record.git_reference.as_deref()
                                                == Some(spec.reference())
                                        }
                                    },
                                    |pin| pin == &record.registry,
                                )
                        })
                });
                if let Some(fixture) = fixture_record {
                    let encoded = fixture
                        .artifact
                        .strip_prefix("base64:")
                        .ok_or("Git fixture artifact must use base64 bytes")?;
                    let bytes = STANDARD
                        .decode(encoded)
                        .map_err(|_| "invalid Git fixture archive encoding")?;
                    record(fixture.registry.clone(), bytes)?
                } else {
                    if using_fixture {
                        return Err("Git fixture has no matching pinned artifact".into());
                    }
                    let reference = pinned
                        .as_ref()
                        .and_then(PackageSource::git_parts)
                        .map(|(_, commit, _)| commit)
                        .unwrap_or(spec.reference());
                    let pinned_spec =
                        GitDependency::parse(&format!("git+{}#{reference}", spec.repository()))?;
                    let staging = scratch()?;
                    let (commit, bytes) = pinned_spec.fetch(&staging.0)?;
                    let source =
                        PackageSource::git(spec.repository().as_str(), &commit, digest(&bytes))
                            .map_err(|error| error.to_string())?;
                    if pinned.as_ref().is_some_and(|pin| pin != &source) {
                        return Err("pinned Git artifact digest mismatch".into());
                    }
                    record(source, bytes)?
                }
            }
        };
        let requirement: Requirement = format!("npm:{}@{}", package.name, package.version)
            .parse::<Requirement>()
            .map_err(|error| error.to_string())?;
        prepared.insert(
            local_name.clone(),
            (
                Dependency::new(package.registry.clone(), local_name, requirement),
                package,
            ),
        );
    }
    Ok(prepared.into_values().collect())
}

fn is_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn matches_source(raw: &str, source: &PackageSource) -> Result<bool, String> {
    Ok(match declaration(raw)? {
        Some(CopiedDeclaration::File(path)) => source
            .file_parts()
            .is_some_and(|(locked, _)| locked == path),
        Some(CopiedDeclaration::Git(spec)) => {
            source.git_parts().is_some_and(|(repository, commit, _)| {
                repository == spec.repository().as_str()
                    && (!is_commit(spec.reference())
                        || spec.reference().eq_ignore_ascii_case(commit))
            })
        }
        None => false,
    })
}

pub(crate) fn validate_locked_files(
    project: &Path,
    lock: &Lockfile,
) -> Result<(), OperationalError> {
    for (key, _) in lock.packages_typed()? {
        if let Some(source) = key.source.copied()
            && let Some((path, expected)) = source.file_parts()
        {
            let bytes = file_bytes(project, path)?;
            if digest(&bytes).as_str() != expected {
                return Err(OperationalError::new(
                    ErrorKind::Integrity,
                    format!("file dependency '{}' digest mismatch", key.name),
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn fetch_pinned(
    project: &Path,
    source: &PackageSource,
    fixture_path: Option<&Path>,
) -> Result<Vec<u8>, OperationalError> {
    let bytes = if let Some((path, _)) = source.file_parts() {
        file_bytes(project, path)?
    } else if let Some((repository, commit, _)) = source.git_parts() {
        if let Some(path) = fixture_path {
            let fixture = fixture(path)?;
            let package = fixture
                .packages
                .iter()
                .find(|package| package.registry == source.as_str())
                .ok_or("Git fixture has no exact pinned artifact")?;
            let encoded = package
                .artifact
                .strip_prefix("base64:")
                .ok_or("Git fixture archive must use base64 bytes")?;
            let bytes = STANDARD
                .decode(encoded)
                .map_err(|_| "invalid Git fixture archive encoding")?;
            if source.artifact_digest() != Some(digest(&bytes).as_str()) {
                return Err(OperationalError::new(
                    ErrorKind::Integrity,
                    "copied artifact digest mismatch",
                ));
            }
            return Ok(bytes);
        }
        let staging = scratch()?;
        let spec = GitDependency::parse(&format!("git+{repository}#{commit}"))?;
        let (actual_commit, bytes) = spec.fetch(&staging.0)?;
        if actual_commit != commit {
            return Err("Git returned a different pinned commit".into());
        }
        bytes
    } else {
        return Err("expected copied artifact source".into());
    };
    if source.artifact_digest() != Some(digest(&bytes).as_str()) {
        return Err(OperationalError::new(
            ErrorKind::Integrity,
            "copied artifact digest mismatch",
        ));
    }
    Ok(bytes)
}

pub(crate) fn validate_copied_root_binding(
    manifest: &PackageManifest,
    local: &str,
    source: Option<&PackageSource>,
) -> Result<bool, String> {
    let declarations = declarations(manifest)
        .filter(|(name, _)| name.as_str() == local)
        .map(|(_, raw)| raw)
        .collect::<Vec<_>>();
    let mut copied = false;
    for raw in &declarations {
        copied |= declaration(raw)?.is_some();
    }
    if !copied {
        return Ok(false);
    }
    let source = source
        .ok_or_else(|| format!("copied dependency '{local}' does not target a copied artifact"))?;
    if source.registry().is_some()
        || declarations
            .iter()
            .any(|raw| !matches_source(raw, source).unwrap_or(false))
    {
        return Err(format!(
            "copied dependency '{local}' does not match its locked source"
        ));
    }
    Ok(true)
}

pub(crate) fn copied_root_names(manifest: &PackageManifest) -> Result<BTreeSet<String>, String> {
    declarations(manifest)
        .filter_map(|(name, raw)| match declaration(raw) {
            Ok(Some(_)) => Some(Ok(name.clone())),
            Ok(None) => None,
            Err(error) => Some(Err(format!("dependency '{name}': {error}"))),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copied_and_registry_declarations_cannot_share_a_local_name() {
        let project = tapid_test_support::TempProject::new("copied-conflict").unwrap();
        project.write("vendor/tool.tgz", b"archive").unwrap();
        let manifest = PackageManifest::parse(r#"{"name":"demo","version":"1.0.0","dependencies":{"tool":"file:vendor/tool.tgz"},"devDependencies":{"tool":"^1"}}"#).unwrap();
        assert!(validate_declarations(project.path(), &manifest).is_err());
    }
}
