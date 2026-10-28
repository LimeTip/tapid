use super::*;

/// Capture a contained directory as canonical tar bytes for subsequent archive
/// verification. Paths are normalized relative to `root`; forbidden roots use
/// the same namespace. This does not publish or trust the resulting bytes.
/// Non-Unix platforms currently return `Unsupported` without reading the source.
/// Writers must stop for a coherent view; capture is not an atomic snapshot.
pub fn capture_directory(
    root: &Path,
    source: &str,
    forbidden: &[&str],
    limits: ArchiveLimits,
) -> Result<Vec<u8>, ExtractError> {
    #[cfg(unix)]
    {
        unix::capture_directory(root, source, forbidden, limits)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, source, forbidden, limits);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory capture is unavailable",
        )
        .into())
    }
}

#[cfg(unix)]
mod unix;

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn directory_capture_is_deterministic_and_pins_bytes_not_timestamps() {
        let project = tapid_test_support::TempProject::new("directory-capture").unwrap();
        project.write("vendor/pkg/package.json", b"{}").unwrap();
        project
            .write("vendor/pkg/index.js", b"module.exports = 1;")
            .unwrap();
        let first =
            capture_directory(project.path(), "vendor/pkg", &[], ArchiveLimits::default()).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&first)),
            "68f33cc97e1c061f907e0a51578bd7f458e137e099fe9811cc637d6014decb86"
        );
        assert_eq!(first.len(), 3072);
        assert_eq!(&first[100..108], b"0000644\0");
        assert_eq!(&first[108..116], b"0000000\0");
        assert_eq!(&first[116..124], b"0000000\0");
        assert_eq!(&first[124..136], b"00000000023\0");
        assert_eq!(&first[136..148], b"00000000000\0");
        assert!(first[first.len() - 1024..].iter().all(|byte| *byte == 0));
        let file = fs::File::options()
            .write(true)
            .open(project.path().join("vendor/pkg/index.js"))
            .unwrap();
        file.set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
        assert_eq!(
            first,
            capture_directory(project.path(), "vendor/pkg", &[], ArchiveLimits::default()).unwrap()
        );
        let output = project.path().join("extracted");
        extract_to(
            &first,
            ArchiveFormat::Tar,
            &output,
            ArchiveLimits::default(),
        )
        .unwrap();
        assert_eq!(
            fs::read(output.join("index.js")).unwrap(),
            b"module.exports = 1;"
        );
        canonical_tree_digest(&output).unwrap();
        project
            .write("vendor/pkg/index.js", b"module.exports = 2;")
            .unwrap();
        assert_ne!(
            first,
            capture_directory(project.path(), "vendor/pkg", &[], ArchiveLimits::default()).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_capture_preserves_empty_directories_modes_and_fixed_exclusions() {
        use std::os::unix::fs::PermissionsExt;
        let project = tapid_test_support::TempProject::new("capture-view").unwrap();
        project.write("pkg/package.json", b"{}").unwrap();
        project.write("pkg/.hidden", b"included").unwrap();
        project.write("pkg/dist/tool", b"included").unwrap();
        project
            .write("pkg/nested/.git/config", b"excluded")
            .unwrap();
        project
            .write("pkg/node_modules/dependency/index.js", b"excluded")
            .unwrap();
        fs::create_dir(project.path().join("pkg/empty")).unwrap();
        fs::set_permissions(
            project.path().join("pkg/dist/tool"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let bytes =
            capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).unwrap();
        let output = project.path().join("output");
        extract_to(
            &bytes,
            ArchiveFormat::Tar,
            &output,
            ArchiveLimits::default(),
        )
        .unwrap();
        assert!(output.join("empty").is_dir());
        assert_eq!(fs::read(output.join(".hidden")).unwrap(), b"included");
        assert!(!output.join("node_modules").exists());
        assert!(!output.join("nested/.git").exists());
        assert_eq!(
            fs::metadata(output.join("dist/tool"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        fs::set_permissions(
            project.path().join("pkg/dist/tool"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert_ne!(
            bytes,
            capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_capture_rejects_unsafe_sources_and_honors_bounds() {
        let project = tapid_test_support::TempProject::new("capture-safety").unwrap();
        project.write("pkg/package.json", b"{}").unwrap();
        for source in [
            "",
            ".",
            "..",
            "../pkg",
            "/pkg",
            "pkg/../pkg",
            "pkg//sub",
            "pkg\\sub",
        ] {
            assert!(
                capture_directory(project.path(), source, &[], ArchiveLimits::default()).is_err(),
                "{source}"
            );
        }
        assert!(
            capture_directory(project.path(), "pkg", &["pkg"], ArchiveLimits::default()).is_err()
        );
        project.write("pkg/managed/data", b"managed").unwrap();
        assert!(
            capture_directory(
                project.path(),
                "pkg",
                &["pkg/managed"],
                ArchiveLimits::default()
            )
            .is_err()
        );
        for limits in [
            ArchiveLimits {
                max_archive_bytes: 10,
                ..Default::default()
            },
            ArchiveLimits {
                validation: ValidationLimits {
                    max_entry_size: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
            ArchiveLimits {
                validation: ValidationLimits {
                    max_entries: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        ] {
            assert!(capture_directory(project.path(), "pkg", &[], limits).is_err());
        }
        project.write("pkg/.tapid-tree", b"forged").unwrap();
        assert!(capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn directory_capture_rejects_links_even_under_excluded_names() {
        let project = tapid_test_support::TempProject::new("capture-links").unwrap();
        project.write("pkg/package.json", b"{}").unwrap();
        for name in ["link", "node_modules", ".git"] {
            let link = project.path().join("pkg").join(name);
            std::os::unix::fs::symlink("package.json", &link).unwrap();
            assert!(
                capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).is_err()
            );
            fs::remove_file(link).unwrap();
        }
        std::os::unix::fs::symlink("pkg", project.path().join("alias")).unwrap();
        assert!(capture_directory(project.path(), "alias", &[], ArchiveLimits::default()).is_err());
        fs::hard_link(
            project.path().join("pkg/package.json"),
            project.path().join("pkg/copy"),
        )
        .unwrap();
        assert!(capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn directory_capture_long_names_roundtrip() {
        let project = tapid_test_support::TempProject::new("capture-long-path").unwrap();
        project.write("pkg/package.json", b"{}").unwrap();
        let name = format!("{}/file", "x".repeat(120));
        project.write(format!("pkg/{name}"), b"long").unwrap();
        let first =
            capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).unwrap();
        assert_eq!(
            first,
            capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).unwrap()
        );
        let output = project.path().join("output");
        extract_to(
            &first,
            ArchiveFormat::Tar,
            &output,
            ArchiveLimits::default(),
        )
        .unwrap();
        assert_eq!(fs::read(output.join(name)).unwrap(), b"long");
    }

    #[cfg(unix)]
    #[test]
    fn directory_capture_rejects_nonportable_entry_names() {
        let project = tapid_test_support::TempProject::new("capture-portable").unwrap();
        project.write("pkg/package.json", b"{}").unwrap();
        for name in ["unsafe?name", "trailing.", "trailing ", "CON", "lpt1.txt"] {
            project.write(format!("pkg/{name}"), b"unsafe").unwrap();
            assert!(
                capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).is_err(),
                "{name}"
            );
            fs::remove_file(project.path().join("pkg").join(name)).unwrap();
        }
    }

    #[test]
    fn directory_capture_rejects_controlled_replacement_and_mutation() {
        for replace in [false, true] {
            let project = tapid_test_support::TempProject::new("capture-race").unwrap();
            project.write("pkg/package.json", b"{}").unwrap();
            project.write("pkg/index.js", b"original").unwrap();
            let path = project.path().join("pkg/index.js");
            unix::before_encoding(move || {
                if replace {
                    let replacement = path.with_extension("replacement");
                    fs::write(&replacement, b"replaced").unwrap();
                    fs::rename(replacement, &path).unwrap();
                } else {
                    fs::write(&path, b"changed length").unwrap();
                }
            });
            let error = capture_directory(project.path(), "pkg", &[], ArchiveLimits::default())
                .unwrap_err();
            assert!(
                error.to_string().contains("changed during capture"),
                "{error}"
            );
            // Failure returns no archive and leaves no publication/staging tree.
            let names: Vec<_> = fs::read_dir(project.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(names, [std::ffi::OsString::from("pkg")]);
            capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).unwrap();
        }
    }

    #[test]
    fn directory_capture_pins_paths_and_entry_kinds() {
        let project = tapid_test_support::TempProject::new("capture-entry-kinds").unwrap();
        project.write("pkg/package.json", b"{}").unwrap();
        project.write("pkg/original", b"").unwrap();
        let original =
            capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).unwrap();
        fs::rename(
            project.path().join("pkg/original"),
            project.path().join("pkg/renamed"),
        )
        .unwrap();
        let renamed =
            capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).unwrap();
        assert_ne!(original, renamed);
        fs::remove_file(project.path().join("pkg/renamed")).unwrap();
        fs::create_dir(project.path().join("pkg/renamed")).unwrap();
        assert_ne!(
            renamed,
            capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).unwrap()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn directory_capture_rejects_fifos() {
        let project = tapid_test_support::TempProject::new("capture-fifo").unwrap();
        project.write("pkg/package.json", b"{}").unwrap();
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            project.path().join("pkg/fifo"),
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        assert!(capture_directory(project.path(), "pkg", &[], ArchiveLimits::default()).is_err());
    }
}
