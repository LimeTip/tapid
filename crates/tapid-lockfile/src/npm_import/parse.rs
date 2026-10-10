use super::*;
use std::fmt;
use tapid_core::RegistryOrigin;
use tapid_resolver::Requirement;

#[derive(Debug)]
pub struct NpmImportError {
    pub pointer: String,
    pub package: String,
    pub field: String,
    pub reason: String,
}
impl fmt::Display for NpmImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: package {}: {}: {}",
            self.pointer, self.package, self.field, self.reason
        )
    }
}
impl std::error::Error for NpmImportError {}
pub(super) fn error(pointer: &str, package: &str, field: &str, reason: &str) -> NpmImportError {
    NpmImportError {
        pointer: pointer.into(),
        package: package.into(),
        field: field.into(),
        reason: reason.into(),
    }
}
fn pointer(base: &str, field: &str) -> String {
    format!("{base}/{}", field.replace('~', "~0").replace('/', "~1"))
}
pub(super) fn json(input: &str) -> Result<Value, NpmImportError> {
    super::json::parse(input).map_err(|e| error("/", "root", "JSON", &e.to_string()))
}
pub(super) fn object<'a>(
    value: &'a Value,
    at: &str,
    identity: &str,
) -> Result<&'a Map<String, Value>, NpmImportError> {
    value
        .as_object()
        .ok_or_else(|| error(at, identity, "object", "expected an object"))
}
fn text<'a>(
    entry: &'a Map<String, Value>,
    field: &str,
    at: &str,
    identity: &str,
) -> Result<&'a str, NpmImportError> {
    entry
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| error(&pointer(at, field), identity, field, "expected a string"))
}
fn flag(
    entry: &Map<String, Value>,
    field: &str,
    at: &str,
    identity: &str,
) -> Result<bool, NpmImportError> {
    match entry.get(field) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(error(
            &pointer(at, field),
            identity,
            field,
            "expected a boolean",
        )),
    }
}
fn strings(
    entry: &Map<String, Value>,
    field: &str,
    at: &str,
    identity: &str,
) -> Result<Vec<String>, NpmImportError> {
    let Some(value) = entry.get(field) else {
        return Ok(Vec::new());
    };
    let values = value.as_array().ok_or_else(|| {
        error(
            &pointer(at, field),
            identity,
            field,
            "expected string array",
        )
    })?;
    values
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|v| !v.is_empty() && !v.chars().any(char::is_whitespace))
                .map(str::to_owned)
                .ok_or_else(|| {
                    error(
                        &pointer(at, field),
                        identity,
                        field,
                        "expected nonempty platform name",
                    )
                })
        })
        .collect()
}
fn requirements(
    entry: &Map<String, Value>,
    field: &str,
    at: &str,
    identity: &str,
) -> Result<BTreeMap<String, Requirement>, NpmImportError> {
    let Some(value) = entry.get(field) else {
        return Ok(BTreeMap::new());
    };
    object(value, &pointer(at, field), identity)?
        .iter()
        .map(|(name, value)| {
            let location = pointer(&pointer(at, field), name);
            name.parse::<PackageName>()
                .map_err(|e| error(&location, identity, field, &e.to_string()))?;
            let raw = value.as_str().ok_or_else(|| {
                error(&location, identity, field, "expected a version requirement")
            })?;
            let requirement = raw
                .parse::<Requirement>()
                .map_err(|e| error(&location, identity, field, &e.to_string()))?;
            Ok((name.clone(), requirement))
        })
        .collect()
}
fn check_fields(
    entry: &Map<String, Value>,
    allowed: &[&str],
    at: &str,
    identity: &str,
) -> Result<(), NpmImportError> {
    for field in entry.keys() {
        if !allowed.contains(&field.as_str()) {
            return Err(error(
                &pointer(at, field),
                identity,
                field,
                "unsupported npm lock field",
            ));
        }
    }
    Ok(())
}
fn placement_name(path: &str) -> Option<&str> {
    let mut rest = path.strip_prefix("node_modules/")?;
    loop {
        let (name, remaining) = if rest.starts_with('@') {
            let slash = rest.find('/')?;
            let end = rest[slash + 1..]
                .find('/')
                .map_or(rest.len(), |i| slash + 1 + i);
            (&rest[..end], &rest[end..])
        } else {
            let end = rest.find('/').unwrap_or(rest.len());
            (&rest[..end], &rest[end..])
        };
        name.parse::<PackageName>().ok()?;
        if remaining.is_empty() {
            return Some(name);
        }
        rest = remaining.strip_prefix("/node_modules/")?;
    }
}
fn lookup(
    packages: &BTreeMap<String, ImportedNpmPackage>,
    parent: &str,
    name: &str,
    peer: bool,
) -> Option<String> {
    let mut base = if peer {
        parent
            .rsplit_once("node_modules/")
            .map_or("", |(base, _)| base.trim_end_matches('/'))
    } else {
        parent
    };
    loop {
        let candidate = if base.is_empty() {
            format!("node_modules/{name}")
        } else {
            format!("{base}/node_modules/{name}")
        };
        if packages.contains_key(&candidate) {
            return Some(candidate);
        }
        if base.is_empty() {
            return None;
        }
        base = base
            .rsplit_once("/node_modules/")
            .map_or("", |(ancestor, _)| ancestor);
    }
}
fn edge(
    packages: &BTreeMap<String, ImportedNpmPackage>,
    parent: &str,
    name: &str,
    requirement: &Requirement,
    peer: bool,
    optional: bool,
    at: &str,
) -> Result<Option<String>, NpmImportError> {
    let Some(target) = lookup(packages, parent, name, peer) else {
        return if optional {
            Ok(None)
        } else {
            Err(error(
                at,
                parent,
                name,
                "selected dependency is missing from npm placements",
            ))
        };
    };
    let selected = &packages[&target];
    if !requirement.matches(&selected.version)
        || requirement.package_name(&name.parse::<PackageName>().expect("validated name"))
            != &selected.name
    {
        return Err(error(
            at,
            parent,
            name,
            "selected package identity/version does not satisfy its declared requirement",
        ));
    }
    Ok(Some(target))
}

pub(super) fn graph(input: &Value) -> Result<ImportedNpmGraph, NpmImportError> {
    let top = object(input, "/", "root")?;
    if top.get("lockfileVersion").and_then(Value::as_u64) != Some(3) {
        return Err(error(
            "/lockfileVersion",
            "root",
            "lockfileVersion",
            "only npm lockfileVersion 3 is supported; regenerate with npm install --package-lock-only --lockfile-version=3, review version changes, then retry",
        ));
    }
    check_fields(
        top,
        &["name", "version", "lockfileVersion", "requires", "packages"],
        "",
        "root",
    )?;
    let entries = object(
        top.get("packages").ok_or_else(|| {
            error(
                "/packages",
                "root",
                "packages",
                "missing package placements",
            )
        })?,
        "/packages",
        "root",
    )?;
    let root = object(
        entries
            .get("")
            .ok_or_else(|| error("/packages/", "root", "root", "missing root entry"))?,
        "/packages/",
        "root",
    )?;
    check_fields(
        root,
        &[
            "name",
            "version",
            "license",
            "engines",
            "dependencies",
            "devDependencies",
            "optionalDependencies",
            "peerDependencies",
            "peerDependenciesMeta",
        ],
        "/packages/",
        "root",
    )?;
    let mut packages = BTreeMap::new();
    for (path, value) in entries.iter().filter(|(path, _)| !path.is_empty()) {
        let at = pointer("/packages", path);
        let entry = object(value, &at, path)?;
        if entry.contains_key("link") {
            return Err(error(
                &pointer(&at, "link"),
                path,
                "link",
                "linked/workspace entries cannot be imported yet; keep the npm lock until workspace conversion is supported",
            ));
        }
        let local_name = placement_name(path).ok_or_else(|| {
            error(
                &at,
                path,
                "path",
                "unsupported package placement; links/workspaces and unsafe paths are unsupported",
            )
        })?;
        let raw_name = if entry.contains_key("name") {
            text(entry, "name", &at, path)?
        } else {
            local_name
        };
        let version = text(entry, "version", &at, path)?;
        let identity = format!("{raw_name}@{version}");
        check_fields(
            entry,
            &[
                "name",
                "version",
                "resolved",
                "integrity",
                "dev",
                "optional",
                "devOptional",
                "peer",
                "license",
                "engines",
                "funding",
                "deprecated",
                "hasInstallScript",
                "bin",
                "dependencies",
                "optionalDependencies",
                "peerDependencies",
                "peerDependenciesMeta",
                "os",
                "cpu",
                "libc",
            ],
            &at,
            &identity,
        )?;
        let name = raw_name
            .parse::<PackageName>()
            .map_err(|e| error(&pointer(&at, "name"), &identity, "name", &e.to_string()))?;
        let version = version.parse::<PackageVersion>().map_err(|e| {
            error(
                &pointer(&at, "version"),
                &identity,
                "version",
                &e.to_string(),
            )
        })?;
        let resolved = text(entry, "resolved", &at, &identity)?;
        crate::validation::validate_artifact_url(resolved).map_err(|_| {
            error(
                &pointer(&at, "resolved"),
                &identity,
                "resolved",
                "expected a safe HTTPS registry tarball URL without credentials, query or fragment",
            )
        })?;
        let url = url::Url::parse(resolved).map_err(|_| {
            error(
                &pointer(&at, "resolved"),
                &identity,
                "resolved",
                "invalid URL",
            )
        })?;
        let registry = url
            .origin()
            .ascii_serialization()
            .parse::<RegistryOrigin>()
            .map_err(|e| {
                error(
                    &pointer(&at, "resolved"),
                    &identity,
                    "resolved",
                    &e.to_string(),
                )
            })?;
        let basename = name.as_str().rsplit('/').next().expect("package name");
        let expected = format!("/{}/-/{basename}-{version}.tgz", name.as_str());
        if !url.path().ends_with(&expected) {
            return Err(error(
                &pointer(&at, "resolved"),
                &identity,
                "resolved",
                "tarball path does not match selected name/version; nonstandard artifact paths are unsupported",
            ));
        }
        let raw_integrity = text(entry, "integrity", &at, &identity)?;
        let integrity = raw_integrity.parse::<PackageIntegrity>().map_err(|_| {
            error(
                &pointer(&at, "integrity"),
                &identity,
                "integrity",
                "requires one canonical padded SHA-512 SRI value",
            )
        })?;
        if raw_integrity != integrity.to_string() {
            return Err(error(
                &pointer(&at, "integrity"),
                &identity,
                "integrity",
                "requires canonical padded SHA-512 SRI",
            ));
        }
        for field in ["dev", "devOptional", "peer", "hasInstallScript"] {
            flag(entry, field, &at, &identity)?;
        }
        packages.insert(
            path.clone(),
            ImportedNpmPackage {
                path: path.clone(),
                name,
                version,
                registry,
                resolved: resolved.into(),
                integrity,
                dependencies: BTreeMap::new(),
                optional_dependencies: BTreeSet::new(),
                peers: BTreeMap::new(),
                optional_peers: BTreeSet::new(),
                optional: flag(entry, "optional", &at, &identity)?,
                os: strings(entry, "os", &at, &identity)?,
                cpu: strings(entry, "cpu", &at, &identity)?,
                libc: strings(entry, "libc", &at, &identity)?,
            },
        );
    }
    for path in packages.keys().cloned().collect::<Vec<_>>() {
        let at = pointer("/packages", &path);
        let entry = entries[&path].as_object().expect("validated entry");
        let mut dependencies = requirements(entry, "dependencies", &at, &path)?;
        let optional = requirements(entry, "optionalDependencies", &at, &path)?;
        dependencies.extend(optional.clone());
        let mut edges = BTreeMap::new();
        for (name, requirement) in dependencies {
            let optional_edge = optional.contains_key(&name);
            let field = if optional_edge {
                "optionalDependencies"
            } else {
                "dependencies"
            };
            if let Some(target) = edge(
                &packages,
                &path,
                &name,
                &requirement,
                false,
                optional_edge,
                &pointer(&pointer(&at, field), &name),
            )? {
                edges.insert(name, target);
            }
        }
        let peers = requirements(entry, "peerDependencies", &at, &path)?;
        let meta = optional_peers(entry, &at, &path, &peers)?;
        let mut peer_edges = BTreeMap::new();
        for (name, requirement) in peers {
            if let Some(target) = edge(
                &packages,
                &path,
                &name,
                &requirement,
                true,
                meta.contains(&name),
                &pointer(&pointer(&at, "peerDependencies"), &name),
            )? {
                if edges.get(&name).is_some_and(|existing| *existing != target) {
                    return Err(error(
                        &at,
                        &path,
                        "peerDependencies",
                        "regular and peer placements conflict",
                    ));
                }
                peer_edges.insert(name, target);
            }
        }
        let package = packages.get_mut(&path).expect("known package");
        package.dependencies = edges;
        package.optional_dependencies = optional.keys().cloned().collect();
        package.peers = peer_edges;
        package.optional_peers = meta;
    }
    let mut requirements = requirements(root, "dependencies", "/packages/", "root")?;
    // Tapid requires overlapping root sections to agree; npm optional overrides are preserved.
    for (name, req) in self::requirements(root, "devDependencies", "/packages/", "root")? {
        if let Some(previous) = requirements.get(&name) {
            let selected = lookup(&packages, "", &name, false)
                .ok_or_else(|| error("/packages/", "root", &name, "missing direct package"))?;
            let selected = &packages[&selected];
            let local_name = name.parse::<PackageName>().expect("validated name");
            if previous.package_name(&local_name) != &selected.name
                || req.package_name(&local_name) != &selected.name
                || !previous.matches(&selected.version)
                || !req.matches(&selected.version)
            {
                return Err(error(
                    "/packages/",
                    "root",
                    &name,
                    "overlapping root requirements disagree with selection",
                ));
            }
        }
        requirements.insert(name, req);
    }
    let optional = self::requirements(root, "optionalDependencies", "/packages/", "root")?;
    requirements.extend(optional.clone());
    let mut roots = BTreeMap::new();
    for (name, req) in requirements {
        if let Some(target) = edge(
            &packages,
            "",
            &name,
            &req,
            false,
            optional.contains_key(&name),
            "/packages/",
        )? {
            roots.insert(name, target);
        }
    }
    let peers = self::requirements(root, "peerDependencies", "/packages/", "root")?;
    let meta = optional_peers(root, "/packages/", "root", &peers)?;
    let mut optional_roots: BTreeSet<_> = optional.keys().cloned().collect();
    for (name, req) in peers {
        if let Some(target) = edge(
            &packages,
            "",
            &name,
            &req,
            true,
            meta.contains(&name),
            "/packages/peerDependencies",
        )? {
            if !roots.contains_key(&name) && meta.contains(&name) {
                optional_roots.insert(name.clone());
            }
            roots.entry(name.clone()).or_insert(target);
        }
    }
    let graph = ImportedNpmGraph {
        packages,
        roots,
        optional_roots,
    };
    validate_representable(&graph)?;
    Ok(graph)
}
fn optional_peers(
    entry: &Map<String, Value>,
    at: &str,
    identity: &str,
    peers: &BTreeMap<String, Requirement>,
) -> Result<BTreeSet<String>, NpmImportError> {
    let mut optional = BTreeSet::new();
    if let Some(value) = entry.get("peerDependenciesMeta") {
        for (name, value) in object(value, &pointer(at, "peerDependenciesMeta"), identity)? {
            let location = pointer(&pointer(at, "peerDependenciesMeta"), name);
            if !peers.contains_key(name) {
                return Err(error(
                    &location,
                    identity,
                    "peerDependenciesMeta",
                    "metadata has no corresponding peer dependency",
                ));
            }
            let meta = object(value, &location, identity)?;
            check_fields(meta, &["optional"], &location, identity)?;
            if flag(meta, "optional", &location, identity)? {
                optional.insert(name.clone());
            }
        }
    }
    Ok(optional)
}

fn validate_representable(graph: &ImportedNpmGraph) -> Result<(), NpmImportError> {
    use crate::LockfilePackageKey;
    let platform = tapid_core::PlatformContext::new(None, None, None).expect("empty context");
    let keys: BTreeMap<_, _> = graph
        .packages
        .iter()
        .map(|(path, package)| {
            (
                path,
                LockfilePackageKey::new(
                    package.registry.clone(),
                    package.name.clone(),
                    package.version.clone(),
                    &graph.peer_context(path),
                    &platform,
                )
                .to_string(),
            )
        })
        .collect();
    let mut reachable = BTreeSet::new();
    let mut pending: Vec<_> = graph.roots.values().cloned().collect();
    while let Some(path) = pending.pop() {
        if !reachable.insert(path.clone()) {
            continue;
        }
        let package = &graph.packages[&path];
        pending.extend(
            package
                .dependencies
                .values()
                .chain(package.peers.values())
                .cloned(),
        );
    }
    for path in graph.packages.keys() {
        if !reachable.contains(path) {
            return Err(error(
                &pointer("/packages", path),
                path,
                "placement",
                "unreachable selected entry cannot be represented; refresh the npm lock before importing",
            ));
        }
    }
    let mut identities = BTreeMap::new();
    let mut artifacts = BTreeMap::new();
    for (path, package) in &graph.packages {
        let at = pointer("/packages", path);
        let artifact_identity = (&package.registry, &package.name, &package.version);
        let artifact = (&package.resolved, &package.integrity);
        if let Some(previous) = artifacts.insert(artifact_identity, artifact)
            && previous != artifact
        {
            return Err(error(
                &pointer(&at, "integrity"),
                &format!("{}@{}", package.name, package.version),
                "resolved/integrity",
                "same registry package/version has conflicting artifact source or integrity",
            ));
        }
        for target in package.peers.values() {
            if package.registry != graph.packages[target].registry {
                return Err(error(
                    &at,
                    path,
                    "peerDependencies",
                    "cross-registry peer context cannot be represented",
                ));
            }
        }
        let mut edges = BTreeMap::new();
        for (name, target) in package.dependencies.iter().chain(&package.peers) {
            if keys[path] == keys[target] {
                return Err(error(
                    &at,
                    path,
                    "dependencies",
                    "self-instance edge cannot be represented",
                ));
            }
            edges.insert(name, &keys[target]);
        }
        let signature = (
            &package.resolved,
            &package.integrity,
            edges,
            &package.os,
            &package.cpu,
            &package.libc,
            &package.optional_dependencies,
            &package.optional_peers,
        );
        if let Some(previous) = identities.insert(&keys[path], signature.clone())
            && previous != signature
        {
            return Err(error(
                &at,
                &format!("{}@{}", package.name, package.version),
                "placement",
                "same Tapid instance has conflicting sources, constraints or dependency placements",
            ));
        }
    }
    Ok(())
}
