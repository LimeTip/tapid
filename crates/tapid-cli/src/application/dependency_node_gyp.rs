//! Invocation-only native builds use the selected Node installation's bundled tool.
//! The copied module closure is bound to its pre-copy digest, never to host PATH.
use super::{OperationalError, error};
use std::fs;
use std::path::Path;

pub(super) const COMMAND: &str =
    "node \"../.tools/npm/node_modules/node-gyp/bin/node-gyp.js\" rebuild";

pub(super) fn prepare(tools: &Path, node: &Path) -> Result<(), OperationalError> {
    let bin = node
        .parent()
        .ok_or_else(|| error("Node runtime has no parent"))?;
    let prefix = bin
        .parent()
        .ok_or_else(|| error("Node runtime has no prefix"))?;
    // Official Windows and Unix Node distributions, including nvm/mise prefixes.
    // A global node-gyp executable or an ambient npm_config_node_gyp is never used.
    let candidates = [
        bin.join("node_modules/npm"),
        prefix.join("lib/node_modules/npm"),
    ];
    let source = candidates.into_iter().find(|root| {
        root.join("node_modules/node-gyp/bin/node-gyp.js").is_file()
    }).ok_or_else(|| error(
        "implicit node-gyp build requires npm's bundled node-gyp beside the selected Node runtime; install a Node distribution that includes npm, or declare an explicit install hook with a pinned toolchain in tapid.lifecycle.toml",
    ))?;
    check_package(&source, "npm")?;
    let modules = source.join("node_modules");
    check_package(&modules.join("node-gyp"), "node-gyp")?;
    reject_reparse_points(&modules)?;
    let digest =
        tapid_archive::canonical_tree_digest_without_root_entry(&modules, ".bin").map_err(error)?;
    let destination = tools.join("npm/node_modules");
    snapshot_modules(&modules, &destination, &digest)?;
    check_package(&destination.join("node-gyp"), "node-gyp")?;
    crate::commands::run::read_bounded_config_file(
        &destination.join("node-gyp/bin/node-gyp.js"),
        1024 * 1024,
    )
    .map_err(|_| {
        error("private node-gyp entry point must be a regular file no larger than 1 MiB")
    })?;
    Ok(())
}

fn check_package(root: &Path, expected: &str) -> Result<(), OperationalError> {
    let bytes =
        crate::commands::run::read_bounded_config_file(&root.join("package.json"), 1024 * 1024)
            .map_err(|_| {
                error(format!(
                    "bundled {expected} package.json must be a regular file no larger than 1 MiB"
                ))
            })?;
    let package: serde_json::Value = serde_json::from_slice(&bytes).map_err(error)?;
    if package["name"].as_str() != Some(expected) {
        return Err(error(format!(
            "bundled node-gyp toolchain has an invalid {expected} identity"
        )));
    }
    Ok(())
}

fn snapshot_modules(
    source: &Path,
    destination: &Path,
    digest: &str,
) -> Result<(), OperationalError> {
    // Store digests deliberately exclude internal tree markers. Host tool bundles
    // cannot carry those markers as unhashed runtime inputs.
    for name in [".tapid-tree", ".tapid-executable-modes"] {
        match fs::symlink_metadata(source.join(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(error(
                    "bundled node-gyp modules contain reserved store metadata",
                ));
            }
        }
    }
    crate::filesystem::tree::copy_tree_contents_without_root_entry(source, destination, ".bin")
        .map_err(error)?;
    reject_reparse_points(destination)?;
    if destination.join(".tapid-tree").exists()
        || destination.join(".tapid-executable-modes").exists()
    {
        return Err(error(
            "bundled node-gyp modules contain reserved store metadata",
        ));
    }
    if tapid_archive::canonical_tree_digest(destination).map_err(error)? != digest {
        return Err(error(
            "bundled node-gyp module tree changed while preparing its private copy",
        ));
    }
    Ok(())
}

// The common tree copier and digest reject symlinks and special files. Windows
// directory junctions additionally require rejecting every reparse point.
fn reject_reparse_points(root: &Path) -> Result<(), OperationalError> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            let metadata = fs::symlink_metadata(&path).map_err(error)?;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(error(
                    "bundled node-gyp module tree contains a reparse point",
                ));
            }
            if metadata.is_dir() {
                for entry in fs::read_dir(&path).map_err(error)? {
                    let entry = entry.map_err(error)?;
                    if path == root && entry.file_name() == ".bin" {
                        continue;
                    }
                    pending.push(entry.path());
                }
            }
        }
    }
    #[cfg(not(windows))]
    let _ = root;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> tapid_test_support::TempProject {
        let project = tapid_test_support::TempProject::new("node-gyp-modules").unwrap();
        project.write("bin/node", b"runtime").unwrap();
        project
            .write("lib/node_modules/npm/package.json", br#"{"name":"npm"}"#)
            .unwrap();
        project
            .write(
                "lib/node_modules/npm/node_modules/node-gyp/package.json",
                br#"{"name":"node-gyp"}"#,
            )
            .unwrap();
        project
            .write(
                "lib/node_modules/npm/node_modules/node-gyp/bin/node-gyp.js",
                b"original",
            )
            .unwrap();
        project
    }

    #[test]
    fn private_node_gyp_modules_survive_host_replacement() {
        let source = bundle();
        let tools = source.path().join("private-tools");
        fs::create_dir(&tools).unwrap();
        prepare(&tools, &source.path().join("bin/node")).unwrap();
        source
            .write(
                "lib/node_modules/npm/node_modules/node-gyp/bin/node-gyp.js",
                b"replaced",
            )
            .unwrap();
        assert_eq!(
            fs::read(tools.join("npm/node_modules/node-gyp/bin/node-gyp.js")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn node_gyp_modules_changed_after_digest_fail_closed() {
        let source = bundle();
        let modules = source.path().join("lib/node_modules/npm/node_modules");
        let digest = tapid_archive::canonical_tree_digest(&modules).unwrap();
        source
            .write(
                "lib/node_modules/npm/node_modules/node-gyp/bin/node-gyp.js",
                b"replaced",
            )
            .unwrap();
        assert!(
            snapshot_modules(&modules, &source.path().join("private-modules"), &digest).is_err()
        );
    }

    #[test]
    fn node_gyp_bundle_rejects_wrong_package_identity() {
        let source = bundle();
        source
            .write(
                "lib/node_modules/npm/node_modules/node-gyp/package.json",
                br#"{"name":"other"}"#,
            )
            .unwrap();
        assert!(
            prepare(
                &source.path().join("private-tools"),
                &source.path().join("bin/node")
            )
            .is_err()
        );
    }

    #[test]
    fn node_gyp_bundle_rejects_unhashed_store_metadata() {
        let source = bundle();
        source
            .write(
                "lib/node_modules/npm/node_modules/.tapid-tree",
                b"unhashed input",
            )
            .unwrap();
        assert!(
            prepare(
                &source.path().join("private-tools"),
                &source.path().join("bin/node")
            )
            .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn node_gyp_bundle_rejects_directory_junctions() {
        use std::os::windows::process::CommandExt;
        let source = bundle();
        let outside = tapid_test_support::TempProject::new("node-gyp-outside").unwrap();
        outside
            .write("package.json", br#"{"name":"node-gyp"}"#)
            .unwrap();
        outside.write("bin/node-gyp.js", b"outside").unwrap();
        let link = source
            .path()
            .join("lib/node_modules/npm/node_modules/node-gyp");
        fs::remove_dir_all(&link).unwrap();
        let status = std::process::Command::new(
            crate::run::windows_system_directory()
                .unwrap()
                .join("cmd.exe"),
        )
        .args(["/D", "/S", "/C"])
        .raw_arg(format!(
            "mklink /J \"{}\" \"{}\"",
            link.to_string_lossy().replace('/', "\\"),
            outside.path().display()
        ))
        .status()
        .unwrap();
        assert!(status.success(), "junction fixture must exist");
        assert!(
            prepare(
                &source.path().join("private-tools"),
                &source.path().join("bin/node")
            )
            .is_err()
        );
        fs::remove_dir(link).unwrap();
        assert_eq!(
            fs::read(outside.path().join("bin/node-gyp.js")).unwrap(),
            b"outside"
        );
    }

    #[cfg(unix)]
    #[test]
    fn node_gyp_bundle_omits_generated_bin_symlinks() {
        let source = bundle();
        source
            .write("lib/node_modules/npm/node_modules/.bin/placeholder", b"")
            .unwrap();
        std::os::unix::fs::symlink(
            "../node-gyp/bin/node-gyp.js",
            source
                .path()
                .join("lib/node_modules/npm/node_modules/.bin/node-gyp"),
        )
        .unwrap();
        let tools = source.path().join("private-tools");
        prepare(&tools, &source.path().join("bin/node")).unwrap();
        assert!(!tools.join("npm/node_modules/.bin").exists());
        assert!(
            tools
                .join("npm/node_modules/node-gyp/bin/node-gyp.js")
                .is_file()
        );
    }

    #[cfg(unix)]
    #[test]
    fn node_gyp_bundle_rejects_links_outside_its_closure() {
        let source = bundle();
        std::os::unix::fs::symlink(
            source.path().join("bin/node"),
            source
                .path()
                .join("lib/node_modules/npm/node_modules/escape"),
        )
        .unwrap();
        assert!(
            prepare(
                &source.path().join("private-tools"),
                &source.path().join("bin/node")
            )
            .is_err()
        );
    }
}
