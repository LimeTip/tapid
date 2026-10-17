use super::{
    ToolBin, error,
    state::{self, Catalog},
};
use crate::application::outcome::OperationalError;
use std::{collections::BTreeMap, path::Path};
use tapid_linker::{ManagedRoot, ShimInterpreter, ShimPackage, plan_shims};
use tapid_manifest::PackageManifest;

pub(super) fn validate_command(command: &str) -> Result<(), OperationalError> {
    if command.is_empty()
        || matches!(command, "." | "..")
        || !command
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(error(
            "tool command names must contain only letters, numbers, dot, underscore, or hyphen",
        ));
    }
    if command.eq_ignore_ascii_case("tapid")
        || cfg!(windows)
            && Path::new(command)
                .with_extension("cmd")
                .file_stem()
                .is_some_and(|s| s.eq_ignore_ascii_case("tapid"))
    {
        return Err(error("tool command collides with Tapid"));
    }
    Ok(())
}
pub(super) fn package_commands(
    root: &Path,
    environment: &Path,
    package_root: &Path,
    manifest: &PackageManifest,
) -> Result<BTreeMap<String, ToolBin>, OperationalError> {
    let plan = plan_shims(
        ManagedRoot::new(root).map_err(error)?,
        vec![ShimPackage {
            tree_root: package_root.to_owned(),
            package_json: manifest.to_json(),
            bin_dir: root.to_owned(),
        }],
        crate::application::replay::current_platform(),
    )
    .map_err(error)?;
    let expected = manifest.bin().map_or(0, |b| b.targets().len());
    if expected == 0 || plan.entries.len() != expected {
        return Err(error(
            "package has no usable bin targets; install-time scripts remain disabled",
        ));
    }
    let mut commands = BTreeMap::new();
    for entry in plan.entries {
        validate_command(&entry.command)?;
        use std::io::Read;
        let mut contents = Vec::new();
        state::open_regular(&entry.source)?
            .take(4096)
            .read_to_end(&mut contents)
            .map_err(error)?;
        let first_line = contents.split(|b| *b == b'\n').next().unwrap_or_default();
        let node = if cfg!(windows) {
            entry.interpreter == ShimInterpreter::Node
        } else {
            // Unix shim plans leave interpreter selection to the target. Keep
            // standalone Node dispatch for the supported Unix shebangs.
            matches!(
                first_line,
                b"#!/usr/bin/env node"
                    | b"#!/usr/bin/env node\r"
                    | b"#!/usr/bin/node"
                    | b"#!/usr/bin/node\r"
            )
        };
        if !node && cfg!(windows) && !contents.starts_with(b"MZ") {
            return Err(error(
                "unsupported tool interpreter; Windows supports Node shebangs and native executables",
            ));
        }
        commands.insert(
            entry.command,
            ToolBin {
                target: entry
                    .source
                    .strip_prefix(environment)
                    .map_err(error)?
                    .to_owned(),
                node,
            },
        );
    }
    Ok(commands)
}
pub(super) fn plan(
    root: &Path,
    catalog: &Catalog,
) -> Result<BTreeMap<String, String>, OperationalError> {
    let mut packages = Vec::new();
    for (name, tool) in &catalog.tools {
        let package_root = root
            .join("environments")
            .join(&tool.generation)
            .join("node_modules")
            .join(name);
        state::check_path(&package_root)?;
        let package_json =
            String::from_utf8(state::read(&package_root.join("package.json"))?).map_err(error)?;
        packages.push(ShimPackage {
            tree_root: package_root,
            package_json,
            bin_dir: root.to_owned(),
        });
    }
    let plan = plan_shims(
        ManagedRoot::new(root).map_err(error)?,
        packages,
        crate::application::replay::current_platform(),
    )
    .map_err(error)?;
    let mut result = BTreeMap::new();
    for entry in plan.entries {
        validate_command(&entry.command)?;
        result.extend(
            crate::filesystem::tool_shims::render(root, &entry.command, entry.strategy)
                .map_err(error)?,
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn windows_tool_bins_reuse_the_planners_node_interpreters() {
        let project = tapid_test_support::TempProject::new("tool-node-interpreters").unwrap();
        let root = super::state::root_path(Some(project.path())).unwrap();
        let package = root.join("node_modules/tool");
        std::fs::create_dir_all(&package).unwrap();
        let manifest = tapid_manifest::PackageManifest::parse(
            r#"{"name":"tool","version":"1.0.0","bin":{"tool":"cli.js"}}"#,
        )
        .unwrap();
        for shebang in [
            "#!node\n",
            "#!node.exe\n",
            "#!/usr/bin/node.exe\n",
            "#!/usr/bin/env node.exe\n",
            "#! /usr/bin/node\r\n",
            "#!/usr/bin/env\tnode\n",
        ] {
            std::fs::write(package.join("cli.js"), shebang).unwrap();
            let commands = super::package_commands(&root, &root, &package, &manifest)
                .unwrap_or_else(|e| panic!("{shebang:?}: {e}"));
            assert!(commands["tool"].node, "{shebang:?}");
        }
        for shebang in ["#!/bin/sh\n", "#!/usr/bin/env node --inspect\n"] {
            std::fs::write(package.join("cli.js"), shebang).unwrap();
            assert!(
                super::package_commands(&root, &root, &package, &manifest).is_err(),
                "{shebang:?}"
            );
        }
    }

    #[test]
    fn large_native_tool_bin_is_planned_without_a_state_file_size_limit() {
        let project = tapid_test_support::TempProject::new("tool-native-prefix").unwrap();
        let root = super::state::root_path(Some(project.path())).unwrap();
        let package = root.join("node_modules/native");
        std::fs::create_dir_all(&package).unwrap();
        let manifest = tapid_manifest::PackageManifest::parse(
            r#"{"name":"native","version":"1.0.0","bin":{"native":"native.exe"}}"#,
        )
        .unwrap();
        let mut binary = vec![0; super::state::LIMIT + 1];
        binary[..2].copy_from_slice(b"MZ");
        std::fs::write(package.join("native.exe"), binary).unwrap();
        let commands = super::package_commands(&root, &root, &package, &manifest).unwrap();
        assert!(!commands["native"].node);
    }
}
