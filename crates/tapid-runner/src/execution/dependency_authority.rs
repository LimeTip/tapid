//! Prepared dependency inputs shared by recipe construction and execution.

use super::{ExecutionError, ExecutionErrorCategory, request::TrustedNodeRuntime};
use crate::LifecycleTool;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Runner-owned declared-tool bindings and the supported backend's system identity.
///
/// This value cannot be constructed from a caller-provided fingerprint or inventory.
/// Windows remains unsupported until its complete runtime input authority is enforced.
/// Clones retain the same bindings; they do not prepare a replacement authority.
///
/// ```compile_fail
/// use tapid_runner::PreparedDependencyAuthority;
/// let forged = PreparedDependencyAuthority("sha256-inventory-only".into());
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedDependencyAuthority(Arc<PreparedInputs>);

#[derive(Debug, Eq, PartialEq)]
struct PreparedInputs {
    system: String,
    identity: String,
    tools: Vec<ToolInput>,
}

#[derive(Debug, Eq, PartialEq)]
struct ToolInput {
    source: PathBuf,
    executable_name: String,
    runtime: TrustedNodeRuntime,
}

impl PreparedDependencyAuthority {
    /// Prepares identity before derived-output lookup. Unsupported backends reject
    /// even an empty inventory, before inspecting caller-supplied tool paths.
    pub fn prepare(tools: &[LifecycleTool]) -> Result<Self, ExecutionError> {
        let system = super::dependency_toolchain_identity()?;
        Self::with_system_identity(tools, &system)
    }

    /// Prepares another approval against the same installation's system identity.
    /// Declared tools are freshly inspected; no caller can replace the system identity.
    pub fn with_tools(&self, tools: &[LifecycleTool]) -> Result<Self, ExecutionError> {
        self.validate()?;
        Self::with_system_identity(tools, &self.0.system)
    }

    fn with_system_identity(tools: &[LifecycleTool], system: &str) -> Result<Self, ExecutionError> {
        if tools.is_empty() || tools.len() > 64 {
            return Err(rejected(
                "prepared dependency authority requires 1 to 64 tools",
            ));
        }
        let mut hash = Sha256::new();
        frame(&mut hash, b"tapid-prepared-dependency-authority-v1");
        frame(&mut hash, system.as_bytes());
        let mut inputs = Vec::with_capacity(tools.len());
        let mut names = std::collections::BTreeSet::new();
        for tool in tools {
            if !names.insert(tool.executable_name().to_ascii_lowercase()) {
                return Err(rejected("duplicate prepared dependency tool name"));
            }
            let source = Path::new(tool.path());
            let canonical = std::fs::canonicalize(source)
                .map_err(|_| rejected("cannot resolve prepared dependency tool"))?;
            let runtime = TrustedNodeRuntime::checked_executable(&canonical)?;
            if runtime.digest_string() != tool.digest() {
                return Err(rejected(
                    "prepared dependency tool does not match approved digest",
                ));
            }
            for value in [tool.name(), tool.path(), tool.digest()] {
                frame(&mut hash, value.as_bytes());
            }
            inputs.push(ToolInput {
                source: source.into(),
                executable_name: tool.executable_name(),
                runtime,
            });
        }
        Ok(Self(Arc::new(PreparedInputs {
            system: system.into(),
            identity: format!("sha256-{:x}", hash.finalize()),
            tools: inputs,
        })))
    }

    /// Identity for generated-output recipes, after revalidating declared tools.
    /// This preserves the existing Linux system-runtime contract; it does not
    /// qualify Windows loader, registry, ARAP, profile or broker inputs.
    pub fn recipe_identity(&self) -> Result<&str, ExecutionError> {
        self.validate()?;
        Ok(&self.0.identity)
    }

    pub(super) fn validate(&self) -> Result<(), ExecutionError> {
        for tool in &self.0.tools {
            if std::fs::canonicalize(&tool.source).ok().as_ref() != Some(&tool.runtime.path) {
                return Err(rejected("prepared dependency tool resolution changed"));
            }
            tool.runtime.validate_identity()?;
        }
        Ok(())
    }

    pub(super) fn validate_executables(
        &self,
        tools: &[TrustedNodeRuntime],
    ) -> Result<(), ExecutionError> {
        self.validate()?;
        if tools.len() != self.0.tools.len()
            || self.0.tools.iter().any(|input| {
                !tools.iter().any(|tool| {
                    tool.path.file_name() == Some(std::ffi::OsStr::new(&input.executable_name))
                        && tool.digest_string() == input.runtime.digest_string()
                })
            })
        {
            return Err(rejected(
                "execution tools differ from prepared dependency authority",
            ));
        }
        Ok(())
    }
}

fn frame(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

fn rejected(message: &str) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCategory::PolicyViolation, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_dependency_authority_identity_is_ordered_and_versioned() {
        let project = tapid_test_support::TempProject::new("authority-order").unwrap();
        let mut tools = fixture_tools(project.path());
        let first =
            PreparedDependencyAuthority::with_system_identity(&tools, "fixture-os").unwrap();
        let repeated =
            PreparedDependencyAuthority::with_system_identity(&tools, "fixture-os").unwrap();
        assert_eq!(
            first.recipe_identity().unwrap(),
            repeated.recipe_identity().unwrap()
        );
        tools.reverse();
        let reordered =
            PreparedDependencyAuthority::with_system_identity(&tools, "fixture-os").unwrap();
        assert_ne!(
            first.recipe_identity().unwrap(),
            reordered.recipe_identity().unwrap()
        );
        let changed_backend =
            PreparedDependencyAuthority::with_system_identity(&tools, "other-os").unwrap();
        assert_ne!(
            reordered.recipe_identity().unwrap(),
            changed_backend.recipe_identity().unwrap()
        );
        assert!(PreparedDependencyAuthority::with_system_identity(&[], "fixture-os").is_err());
        assert!(
            PreparedDependencyAuthority::with_system_identity(
                &[tools[0].clone(), tools[0].clone()],
                "fixture-os"
            )
            .is_err()
        );
        assert!(
            PreparedDependencyAuthority::with_system_identity(
                &vec![tools[0].clone(); 65],
                "fixture-os"
            )
            .is_err()
        );
    }

    #[test]
    fn prepared_dependency_authority_rejects_replacement_execution_tools() {
        let project = tapid_test_support::TempProject::new("authority-execution").unwrap();
        let tools = fixture_tools(project.path());
        let authority =
            PreparedDependencyAuthority::with_system_identity(&tools, "fixture-os").unwrap();
        let mut builder = crate::ExecutionRequest::builder(tools[0].path())
            .project_root(project.path())
            .dependency_authority(&authority);
        for tool in &tools {
            // Executed snapshots must use the approved executable names.
            let destination = project.path().join(tool.executable_name());
            if destination != std::path::Path::new(tool.path()) {
                std::fs::copy(tool.path(), &destination).unwrap();
            }
            builder = builder
                .verified_executable(std::fs::canonicalize(destination).unwrap(), tool.digest());
        }
        let request = builder.build().unwrap();
        let cloned = request.clone();
        drop(request);
        cloned.validate().unwrap();
        let omitted = crate::ExecutionRequest::builder(tools[0].path())
            .project_root(project.path())
            .dependency_authority(&authority)
            .verified_executable(
                std::fs::canonicalize(tools[0].path()).unwrap(),
                tools[0].digest(),
            )
            .build()
            .unwrap_err();
        assert_eq!(
            omitted.category(),
            crate::ExecutionErrorCategory::PolicyViolation
        );
        let replacement_directory = project.path().join("replacement");
        std::fs::create_dir(&replacement_directory).unwrap();
        let replacement = replacement_directory.join(tools[1].executable_name());
        std::fs::copy(tools[1].path(), &replacement).unwrap();
        std::fs::write(&replacement, b"unapproved replacement").unwrap();
        let replacement_digest = format!("sha256-{:x}", Sha256::digest(b"unapproved replacement"));
        assert!(
            crate::ExecutionRequest::builder(tools[0].path())
                .project_root(project.path())
                .dependency_authority(&authority)
                .verified_executable(
                    std::fs::canonicalize(tools[0].path()).unwrap(),
                    tools[0].digest()
                )
                .verified_executable(
                    std::fs::canonicalize(replacement).unwrap(),
                    replacement_digest
                )
                .build()
                .is_err()
        );
    }

    #[test]
    #[cfg(unix)]
    fn prepared_dependency_authority_rejects_changed_source_resolution() {
        use std::os::unix::fs::symlink;
        let project = tapid_test_support::TempProject::new("authority-source-resolution").unwrap();
        let mut tools = fixture_tools(project.path());
        let alias = project.path().join("alias");
        symlink(tools[0].path(), &alias).unwrap();
        tools[0] = toml::from_str(&format!(
            "name = 'sh'\npath = '{}'\ndigest = '{}'",
            alias.display(),
            tools[0].digest()
        ))
        .unwrap();
        let authority =
            PreparedDependencyAuthority::with_system_identity(&tools, "fixture-os").unwrap();
        std::fs::remove_file(&alias).unwrap();
        assert!(authority.recipe_identity().is_err());
        // Even identical bytes at a different native object cannot replace the prepared source.
        symlink(tools[1].path(), &alias).unwrap();
        assert!(authority.recipe_identity().is_err());
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn prepared_dependency_authority_rejects_incomplete_backend_before_tool_inspection() {
        let project = tapid_test_support::TempProject::new("authority-incomplete").unwrap();
        let tools = fixture_tools(project.path());
        std::fs::remove_file(tools[0].path()).unwrap();
        assert_eq!(
            PreparedDependencyAuthority::prepare(&[])
                .unwrap_err()
                .category(),
            crate::ExecutionErrorCategory::UnsupportedContainment
        );
        assert_eq!(
            PreparedDependencyAuthority::prepare(&tools)
                .unwrap_err()
                .category(),
            crate::ExecutionErrorCategory::UnsupportedContainment
        );
    }

    #[test]
    #[cfg(unix)]
    fn prepared_dependency_authority_rejects_secondary_tool_mutation() {
        let project = tapid_test_support::TempProject::new("prepared-authority").unwrap();
        let tools = fixture_tools(project.path());
        let authority =
            PreparedDependencyAuthority::with_system_identity(&tools, "fixture-os").unwrap();
        let original_identity = authority.recipe_identity().unwrap().to_owned();
        let clone = authority.clone();
        std::fs::write(tools[1].path(), b"changed secondary tool").unwrap();
        assert!(clone.validate().is_err());
        let mut changed = tools.clone();
        changed[1] = toml::from_str(&format!(
            "name = 'secondary'\npath = '{}'\ndigest = 'sha256-{:x}'",
            tools[1].path(),
            Sha256::digest(b"changed secondary tool")
        ))
        .unwrap();
        let refreshed =
            PreparedDependencyAuthority::with_system_identity(&changed, "fixture-os").unwrap();
        assert_ne!(original_identity, refreshed.recipe_identity().unwrap());
    }

    fn fixture_tools(root: &std::path::Path) -> Vec<crate::LifecycleTool> {
        use sha2::{Digest, Sha256};
        let digest = format!("sha256-{:x}", Sha256::digest(b"fixture"));
        ["sh", "secondary"]
            .into_iter()
            .map(|name| {
                let path = root.join(name);
                std::fs::write(&path, b"fixture").unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                }
                let path = path.to_str().unwrap().replace('\\', "\\\\");
                toml::from_str(&format!(
                    "name = '{name}'\npath = \"{path}\"\ndigest = '{digest}'\n"
                ))
                .unwrap()
            })
            .collect()
    }
}
