//! Convert the bounded, root-hoisted workspace graph into registry entry points.
//! The original npm placements and local identities remain in the imported lock.
use super::parse::*;
use super::*;

pub(super) fn graph(input: &Value) -> Result<Option<ImportedNpmGraph>, NpmImportError> {
    let Some(entries) = input.get("packages").and_then(Value::as_object) else {
        return Ok(None);
    };
    if !entries.values().any(|entry| entry.get("link").is_some()) {
        return Ok(None);
    }
    let mut workspaces = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut normalized = input.clone();
    for (path, value) in entries {
        let Some(entry) = value.as_object().filter(|entry| entry.contains_key("link")) else {
            continue;
        };
        let at = pointer("/packages", path);
        if entry.contains_key("version") || entry.contains_key("integrity") {
            return Err(error(
                &pointer(&at, "link"),
                path,
                "link",
                "local links cannot carry registry artifact fields",
            ));
        }
        check_fields(entry, &["resolved", "link"], &at, path)?;
        if !flag(entry, "link", &at, path)? {
            return Err(error(&pointer(&at, "link"), path, "link", "expected true"));
        }
        let target = text(entry, "resolved", &at, path)?;
        let member_at = pointer("/packages", target);
        let member = object(
            entries
                .get(target)
                .ok_or_else(|| error(&at, path, "resolved", "missing local target"))?,
            &member_at,
            target,
        )?;
        let name = if member.contains_key("name") {
            text(member, "name", &member_at, target)?
        } else {
            placement_name(path)
                .ok_or_else(|| error(&at, path, "path", "invalid local package placement"))?
        };
        let version = text(member, "version", &member_at, target)?;
        let source = crate::LocalWorkspaceSource::new(target, name, version)
            .map_err(|e| error(&at, path, "resolved", &e.to_string()))?;
        if source.path() != target
            || target
                .split('/')
                .any(|component| component == "node_modules")
            || path != &format!("node_modules/{name}")
        {
            return Err(error(
                &at,
                path,
                "link",
                "only root links to named contained workspace members are supported",
            ));
        }
        check_fields(
            member,
            &[
                "name",
                "version",
                "license",
                "engines",
                "bin",
                "hasInstallScript",
                "dependencies",
                "devDependencies",
                "optionalDependencies",
                "peerDependencies",
                "peerDependenciesMeta",
            ],
            &member_at,
            name,
        )?;
        flag(member, "hasInstallScript", &member_at, name)?;
        if names
            .insert(
                name.to_owned(),
                version
                    .parse::<PackageVersion>()
                    .map_err(|e| error(&member_at, name, "version", &e.to_string()))?,
            )
            .is_some()
        {
            return Err(error(&at, path, "link", "duplicate local package name"));
        }
        let mut selected = member.clone();
        selected.insert("name".into(), Value::String(name.to_owned()));
        workspaces.insert(target.to_owned(), Value::Object(selected));
        normalized["packages"]
            .as_object_mut()
            .expect("entries")
            .remove(path);
        normalized["packages"]
            .as_object_mut()
            .expect("entries")
            .remove(target);
    }
    let mut required = BTreeMap::new();
    let mut optional = BTreeMap::new();
    let mut checks = Vec::new();
    for (path, entry) in std::iter::once(("", &entries[""])).chain(
        workspaces
            .iter()
            .map(|(path, entry)| (path.as_str(), entry)),
    ) {
        let at = pointer("/packages", path);
        let entry = object(entry, &at, path)?;
        let peers = requirements(entry, "peerDependencies", &at, path)?;
        let meta = optional_peers(entry, &at, path, &peers)?;
        for field in [
            "dependencies",
            "devDependencies",
            "optionalDependencies",
            "peerDependencies",
        ] {
            for (name, requirement) in requirements(entry, field, &at, path)? {
                // npm optional dependencies override the same production declaration.
                if field == "dependencies"
                    && entry
                        .get("optionalDependencies")
                        .and_then(Value::as_object)
                        .is_some_and(|optional| optional.contains_key(&name))
                {
                    continue;
                }
                let location = pointer(&pointer(&at, field), &name);
                if let Some(version) = names.get(&name) {
                    let local = name.parse::<PackageName>().expect("validated name");
                    if requirement.package_name(&local) != &local || !requirement.matches(version) {
                        return Err(error(
                            &location,
                            path,
                            field,
                            "local workspace identity/version does not satisfy requirement",
                        ));
                    }
                    continue;
                }
                let raw = entry[field][&name].clone();
                let is_optional = field == "optionalDependencies"
                    || field == "peerDependencies" && meta.contains(&name);
                if is_optional {
                    optional.insert(name.clone(), raw);
                } else {
                    required.insert(name.clone(), raw);
                }
                checks.push((name, requirement, is_optional, location, path.to_owned()));
            }
        }
    }
    for name in required.keys() {
        optional.remove(name);
    }
    let normalized_root = normalized["packages"][""].as_object_mut().expect("root");
    for field in [
        "workspaces",
        "devDependencies",
        "peerDependencies",
        "peerDependenciesMeta",
    ] {
        normalized_root.remove(field);
    }
    normalized_root.insert(
        "dependencies".into(),
        serde_json::to_value(required).expect("map"),
    );
    normalized_root.insert(
        "optionalDependencies".into(),
        serde_json::to_value(optional).expect("map"),
    );
    let registry_placements: BTreeMap<_, _> = normalized["packages"]
        .as_object()
        .expect("entries")
        .keys()
        .map(|path| (path.clone(), ()))
        .collect();
    for (path, entry) in normalized["packages"].as_object().expect("entries") {
        if path.is_empty() {
            continue;
        }
        let at = pointer("/packages", path);
        let entry = object(entry, &at, path)?;
        for field in ["dependencies", "optionalDependencies", "peerDependencies"] {
            for name in requirements(entry, field, &at, path)?.keys() {
                if names.contains_key(name)
                    && lookup(
                        &registry_placements,
                        path,
                        name,
                        field == "peerDependencies",
                    )
                    .is_none()
                {
                    return Err(error(
                        &pointer(&pointer(&at, field), name),
                        path,
                        field,
                        "registry edges to local workspace members cannot be represented",
                    ));
                }
            }
        }
    }
    let mut graph = parse::graph(&normalized)?;
    for (name, requirement, optional, location, path) in checks {
        edge(
            &graph.packages,
            "",
            &name,
            &requirement,
            false,
            optional,
            &location,
        )
        .map_err(|mut e| {
            e.package = path;
            e
        })?;
    }
    // Every member is installed, even if the root has no direct dependency on it.
    graph.workspaces = workspaces;
    Ok(Some(graph))
}

pub(super) fn validate_manifests(
    graph: &ImportedNpmGraph,
    manifests: &BTreeMap<String, (String, String)>,
) -> Result<BTreeMap<String, String>, NpmImportError> {
    if graph.workspaces.keys().ne(manifests.keys()) {
        return Err(error(
            "/packages",
            "root",
            "workspaces",
            "local targets do not match discovered workspace membership",
        ));
    }
    let mut digests = BTreeMap::new();
    for (path, (text, digest)) in manifests {
        let current = json(text)?;
        let current = object(&current, &pointer("/packages", path), path)?;
        let selected = graph.workspaces[path]
            .as_object()
            .expect("validated member");
        validate_manifest_entry(
            selected,
            current,
            &[
                "name",
                "version",
                "dependencies",
                "devDependencies",
                "optionalDependencies",
                "peerDependencies",
                "peerDependenciesMeta",
                "bin",
            ],
            &pointer("/packages", path),
            path,
        )?;
        digest
            .parse::<ArtifactDigest>()
            .map_err(|e| error("/workspaceManifestDigests", path, "digest", &e.to_string()))?;
        digests.insert(path.clone(), digest.clone());
    }
    Ok(digests)
}
