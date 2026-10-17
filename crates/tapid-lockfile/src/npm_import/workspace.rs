//! Validate local workspace identities and their npm links.
//! The original npm placements and local identities remain in the imported lock.
use super::parse::*;
use super::*;

pub(super) fn members(input: &Value) -> Result<BTreeMap<String, Value>, NpmImportError> {
    let Some(entries) = input.get("packages").and_then(Value::as_object) else {
        return Ok(BTreeMap::new());
    };
    if !entries.values().any(|entry| entry.get("link").is_some()) {
        return Ok(BTreeMap::new());
    }
    let mut workspaces = BTreeMap::new();
    let mut names = BTreeMap::new();
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
        let placement = path
            .rsplit_once("/node_modules/")
            .map_or(path.as_str(), |(_, suffix)| suffix);
        let placement = if placement.starts_with("node_modules/") {
            placement.to_owned()
        } else {
            format!("node_modules/{placement}")
        };
        let name = if member.contains_key("name") {
            text(member, "name", &member_at, target)?
        } else {
            placement_name(&placement)
                .ok_or_else(|| error(&at, path, "path", "invalid local package placement"))?
        };
        let version = text(member, "version", &member_at, target)?;
        let source = crate::LocalWorkspaceSource::new(target, name, version)
            .map_err(|e| error(&at, path, "resolved", &e.to_string()))?;
        if source.path() != target
            || target
                .split('/')
                .any(|component| component == "node_modules")
        {
            return Err(error(
                &at,
                path,
                "link",
                "local targets must be named contained workspace members",
            ));
        }
        check_fields(
            member,
            &[
                "name",
                "version",
                "license",
                "funding",
                "deprecated",
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
        if let Some(previous) = names.insert(name.to_owned(), target.to_owned())
            && previous != target
        {
            return Err(error(&at, path, "link", "duplicate local package name"));
        }
        let mut selected = member.clone();
        selected.insert("name".into(), Value::String(name.to_owned()));
        workspaces.insert(target.to_owned(), Value::Object(selected));
    }
    Ok(workspaces)
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
            ],
            &pointer("/packages", path),
            path,
        )?;
        if normalized_bin(selected, path)? != normalized_bin(current, path)? {
            return Err(error(
                &pointer("/packages", path),
                path,
                "bin",
                "does not match package.json; regenerate package-lock.json with npm before importing",
            ));
        }
        digest
            .parse::<ArtifactDigest>()
            .map_err(|e| error("/workspaceManifestDigests", path, "digest", &e.to_string()))?;
        digests.insert(path.clone(), digest.clone());
    }
    Ok(digests)
}

fn normalized_bin(
    entry: &Map<String, Value>,
    path: &str,
) -> Result<Option<tapid_manifest::PackageBin>, NpmImportError> {
    let Some(bin) = entry
        .get("bin")
        .filter(|v| !v.as_object().is_some_and(Map::is_empty))
    else {
        return Ok(None);
    };
    // Reuse manifest bin validation without interpreting unrelated npm metadata.
    let manifest = serde_json::json!({"name":entry["name"],"version":entry["version"],"bin":bin});
    tapid_manifest::PackageManifest::parse(&manifest.to_string())
        .map(|manifest| manifest.bin().cloned())
        .map_err(|e| error(&pointer("/packages", path), path, "bin", &e.to_string()))
}
