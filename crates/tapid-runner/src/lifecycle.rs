//! Exact, non-interactive authorization for dependency installation hooks.
use crate::{ExecutionLimits, FilesystemPolicy, SandboxMode, SandboxPolicy};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use tapid_core::{ArtifactDigest, PackageIntegrity, PackageName, PackageVersion};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    #[serde(default)]
    approvals: Vec<RawApproval>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawApproval {
    package: String,
    version: String,
    hook: String,
    archive_digest: Option<String>,
    script_digest: Option<String>,
    read: Option<Vec<String>>,
    write: Option<Vec<String>>,
    network: Option<bool>,
    environment: Option<BTreeMap<String, String>>,
    timeout_seconds: Option<u64>,
    max_output_bytes: Option<u64>,
    max_processes: Option<u32>,
    max_memory_bytes: Option<u64>,
    tools: Option<Vec<LifecycleTool>>,
    system_toolchain: Option<bool>,
    #[serde(default)]
    dependencies: bool,
    #[serde(default)]
    process_memory_stats: bool,
}

/// One explicitly pinned executable copied into private read-only tool storage.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleTool {
    name: String,
    path: String,
    digest: String,
}
impl LifecycleTool {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// Validated approval. Fields cannot be changed after parsing.
#[derive(Clone, Debug)]
pub struct DependencyLifecycleApproval {
    package: String,
    version: String,
    archive_digest: String,
    script_digest: String,
    hook: String,
    policy: SandboxPolicy,
    environment: BTreeMap<String, String>,
    tools: Vec<LifecycleTool>,
    dependencies: bool,
    process_memory_stats: bool,
}
impl DependencyLifecycleApproval {
    pub fn package(&self) -> &str {
        &self.package
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn hook(&self) -> &str {
        &self.hook
    }
    pub fn archive_digest(&self) -> &str {
        &self.archive_digest
    }
    pub fn script_digest(&self) -> &str {
        &self.script_digest
    }
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }
    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }
    pub fn tools(&self) -> &[LifecycleTool] {
        &self.tools
    }
    pub fn dependencies(&self) -> bool {
        self.dependencies
    }
    pub fn process_memory_stats(&self) -> bool {
        self.process_memory_stats
    }
    pub fn validate_script(&self, script: &str) -> Result<(), String> {
        if self.script_digest != sha256(script.as_bytes()) {
            return Err("dependency lifecycle script digest does not match approval".into());
        }
        Ok(())
    }
}

/// Checked-in dependency policy; an absent document means no approvals.
#[derive(Clone, Debug)]
pub struct DependencyLifecyclePolicy {
    digest: String,
    approvals: Vec<DependencyLifecycleApproval>,
}
impl DependencyLifecyclePolicy {
    pub fn denied() -> Self {
        Self {
            digest: sha256(b"tapid-lifecycle-denied-v1"),
            approvals: Vec::new(),
        }
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > crate::config::MAX_CONFIG_BYTES {
            return Err("dependency lifecycle policy exceeds 1 MiB".into());
        }
        let input =
            std::str::from_utf8(bytes).map_err(|_| "dependency lifecycle policy must be UTF-8")?;
        let document: Document =
            toml::from_str(input).map_err(|_| "invalid dependency lifecycle policy")?;
        if document.schema != 1 || document.approvals.len() > crate::config::MAX_PROFILE_COUNT {
            return Err(
                "unsupported dependency lifecycle policy schema or too many approvals".into(),
            );
        }
        let mut approvals = Vec::new();
        let mut identities = BTreeSet::new();
        for raw in document.approvals {
            if !matches!(raw.hook.as_str(), "preinstall" | "install" | "postinstall") {
                return Err(format!(
                    "unsupported dependency lifecycle hook {}",
                    raw.hook
                ));
            }
            if raw.system_toolchain != Some(true) {
                return Err("lifecycle system-toolchain = true must explicitly approve the documented read-only system runtime paths".into());
            }
            raw.package
                .parse::<PackageName>()
                .map_err(|_| "invalid lifecycle package name")?;
            raw.version
                .parse::<PackageVersion>()
                .map_err(|_| "lifecycle version must be exact")?;
            let archive_digest = raw
                .archive_digest
                .ok_or("lifecycle archive-digest is required")?;
            let canonical = archive_digest
                .parse::<PackageIntegrity>()
                .map_err(|_| "lifecycle archive-digest must be SHA-512 SRI")?
                .to_string();
            if canonical != archive_digest {
                return Err("lifecycle archive-digest must be canonical".into());
            }
            let script_digest = raw
                .script_digest
                .ok_or("lifecycle script-digest is required")?;
            check_sha256(&script_digest)?;
            if !identities.insert((raw.package.clone(), raw.version.clone(), raw.hook.clone())) {
                return Err("duplicate dependency lifecycle approval".into());
            }
            let filesystem = FilesystemPolicy::new(
                raw.read.ok_or("lifecycle read is required")?,
                raw.write.ok_or("lifecycle write is required")?,
            )
            .map_err(|error| error.to_string())?;
            let environment = raw.environment.ok_or("lifecycle environment is required")?;
            if environment.iter().any(|(name, value)| {
                name.len() > 255
                    || value.len() > 4096
                    || value.contains('\0')
                    || sensitive_environment(name)
            }) {
                return Err(
                    "dependency lifecycle environment contains a reserved or credential variable"
                        .into(),
                );
            }
            let limits = ExecutionLimits::new(
                Some(
                    raw.timeout_seconds
                        .ok_or("lifecycle timeout-seconds is required")?,
                ),
                Some(
                    raw.max_output_bytes
                        .ok_or("lifecycle max-output-bytes is required")?,
                ),
                Some(
                    raw.max_processes
                        .ok_or("lifecycle max-processes is required")?,
                ),
                Some(
                    raw.max_memory_bytes
                        .ok_or("lifecycle max-memory-bytes is required")?,
                ),
            )
            .map_err(|error| error.to_string())?;
            let policy = SandboxPolicy::new(
                SandboxMode::Required,
                filesystem,
                raw.network.ok_or("lifecycle network is required")?,
                environment.keys().cloned().collect(),
                true,
                limits,
            )
            .map_err(|error| error.to_string())?;
            let tools = raw.tools.ok_or("lifecycle tools are required")?;
            if tools.is_empty() || tools.len() > 64 {
                return Err("lifecycle tools must contain between 1 and 64 executables".into());
            }
            let mut names = BTreeSet::new();
            for tool in &tools {
                if tool.name.is_empty()
                    || tool.name.len() > 255
                    || !tool
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                    || tool.name == "."
                    || tool.name == ".."
                    || !names.insert(&tool.name)
                {
                    return Err("invalid or duplicate lifecycle tool name".into());
                }
                if tool.path.len() > 4096
                    || !std::path::Path::new(&tool.path).is_absolute()
                    || tool.path.contains('\0')
                {
                    return Err("lifecycle tool path must be absolute".into());
                }
                check_sha256(&tool.digest)?;
            }
            if !names.contains(&"sh".to_owned()) {
                return Err("lifecycle tools must pin sh".into());
            }
            approvals.push(DependencyLifecycleApproval {
                package: raw.package,
                version: raw.version,
                hook: raw.hook,
                archive_digest,
                script_digest,
                policy,
                environment,
                tools,
                dependencies: raw.dependencies,
                process_memory_stats: raw.process_memory_stats,
            });
        }
        Ok(Self {
            digest: sha256(bytes),
            approvals,
        })
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn approvals(&self) -> &[DependencyLifecycleApproval] {
        &self.approvals
    }
    pub fn approval_for(
        &self,
        package: &str,
        version: &str,
        archive_digest: &str,
        hook: &str,
        script: &str,
    ) -> Result<Option<&DependencyLifecycleApproval>, String> {
        let candidates = self
            .approvals
            .iter()
            .filter(|a| a.package == package && a.hook == hook)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(None);
        }
        let approval = candidates.iter().copied().find(|a| a.version == version).ok_or_else(|| format!("dependency lifecycle approval does not match version/archive for {package}@{version} {hook}"))?;
        if approval.version != version || approval.archive_digest != archive_digest {
            return Err(format!(
                "dependency lifecycle approval does not match version/archive for {package}@{version} {hook}"
            ));
        }
        approval.validate_script(script)?;
        Ok(Some(approval))
    }
    /// Bind derived output to source, the entire policy, exact script, toolchain,
    /// and the dependency graph visible to the hook.
    pub fn derived_key(
        &self,
        approval: &DependencyLifecycleApproval,
        toolchain: &str,
        graph: &str,
    ) -> String {
        let mut hash = Sha256::new();
        for value in [
            "tapid-derived-lifecycle-v1",
            &approval.package,
            &approval.version,
            &approval.archive_digest,
            &self.digest,
            &approval.hook,
            &approval.script_digest,
            toolchain,
            graph,
        ] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
        format!("sha256-{:x}", hash.finalize())
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256-{:x}", Sha256::digest(bytes))
}
fn check_sha256(digest: &str) -> Result<(), String> {
    let parsed = digest
        .parse::<ArtifactDigest>()
        .map_err(|_| "lifecycle digest must be SHA-256")?;
    if parsed.to_string() != digest || !digest.starts_with("sha256-") {
        return Err("lifecycle digest must be canonical SHA-256".into());
    }
    Ok(())
}
fn sensitive_environment(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    if name.starts_with("LD_") || name.starts_with("DYLD_") {
        return true;
    }
    matches!(
        name.as_str(),
        "HOME"
            | "USERPROFILE"
            | "PATH"
            | "SSH_AUTH_SOCK"
            | "GCONV_PATH"
            | "GLIBC_TUNABLES"
            | "NODE_OPTIONS"
            | "BASH_ENV"
            | "ENV"
    ) || [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "CREDENTIAL",
        "AWS_",
        "AZURE_",
        "GOOGLE_",
        "GITHUB_",
        "NPM_",
        "DEPLOY",
    ]
    .iter()
    .any(|part| name.contains(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> String {
        format!(
            r#"schema = 1
[[approvals]]
package = "native-demo"
system-toolchain = true
version = "1.0.0"
archive-digest = "sha512-{}"
hook = "postinstall"
script-digest = "{}"
read = ["."]
write = ["build"]
network = false
environment = {{ NODE_ENV = "production" }}
timeout-seconds = 10
max-output-bytes = 1024
max-processes = 32
max-memory-bytes = 134217728
tools = [{{ name = "sh", path = "/bin/sh", digest = "sha256-{}" }}]
"#,
            "A".repeat(86) + "==",
            sha256(b"echo build"),
            "0".repeat(64)
        )
    }

    #[test]
    fn approval_requires_exact_version_archive_and_script() {
        let policy = DependencyLifecyclePolicy::parse(document().as_bytes()).unwrap();
        let archive = policy.approvals()[0].archive_digest();
        assert!(
            policy
                .approval_for("native-demo", "1.0.0", archive, "postinstall", "echo build")
                .unwrap()
                .is_some()
        );
        assert!(
            policy
                .approval_for("native-demo", "1.0.1", archive, "postinstall", "echo build")
                .is_err()
        );
        assert!(
            policy
                .approval_for(
                    "native-demo",
                    "1.0.0",
                    "changed",
                    "postinstall",
                    "echo build"
                )
                .is_err()
        );
        assert!(
            policy
                .approval_for(
                    "native-demo",
                    "1.0.0",
                    archive,
                    "postinstall",
                    "echo changed"
                )
                .is_err()
        );
        assert!(
            policy
                .approval_for("other", "1.0.0", archive, "postinstall", "echo build")
                .unwrap()
                .is_none()
        );
        assert!(
            policy
                .approval_for("native-demo", "1.0.0", archive, "install", "echo build")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn approvals_reject_loader_controls_before_launch() {
        let document = document();
        for name in [
            "LD_AUDIT",
            "LD_BIND_NOW",
            "LD_DEBUG_OUTPUT",
            "ld_future_option",
            "DYLD_INSERT_LIBRARIES",
            "dyld_library_path",
            "GCONV_PATH",
            "GLIBC_TUNABLES",
        ] {
            let changed = document.replace("NODE_ENV", name);
            assert!(
                DependencyLifecyclePolicy::parse(changed.as_bytes()).is_err(),
                "accepted {name}"
            );
        }
        assert!(DependencyLifecyclePolicy::parse(document.as_bytes()).is_ok());
    }

    #[test]
    fn approvals_require_limits_and_reject_unknown_or_unsafe_authority() {
        let document = document();
        for changed in [
            document.replace("timeout-seconds = 10\n", ""),
            document.replace("max-output-bytes = 1024", "max-output-bytes = 0"),
            document.replace("write = [\"build\"]", "write = [\"../escape\"]"),
            document.replace("NODE_ENV", "npm_token"),
            document.replace("NODE_ENV", "AWS_ACCESS_KEY_ID"),
            document.replace("NODE_ENV", "PATH"),
            document.replace("postinstall", "prepare"),
            document.replace("network = false", "network = false\nunknown = true"),
            document.replace("1.0.0", "^1.0.0"),
            document.replace("name = \"sh\"", "name = \"../sh\""),
        ] {
            assert!(
                DependencyLifecyclePolicy::parse(changed.as_bytes()).is_err(),
                "{changed}"
            );
        }
    }

    #[test]
    fn derived_identity_changes_with_policy_script_source_toolchain_and_graph() {
        let input = document();
        let policy = DependencyLifecyclePolicy::parse(input.as_bytes()).unwrap();
        let approval = &policy.approvals()[0];
        let original = policy.derived_key(approval, "toolchain-1", "graph-1");
        assert_ne!(
            original,
            policy.derived_key(approval, "toolchain-2", "graph-1")
        );
        assert_ne!(
            original,
            policy.derived_key(approval, "toolchain-1", "graph-2")
        );
        for changed in [
            input.replace("timeout-seconds = 10", "timeout-seconds = 11"),
            input.replace(&sha256(b"echo build"), &sha256(b"echo changed")),
            input.replace(&("A".repeat(86) + "=="), &("B".repeat(85) + "A==")),
        ] {
            let policy = DependencyLifecyclePolicy::parse(changed.as_bytes()).unwrap();
            assert_ne!(
                original,
                policy.derived_key(&policy.approvals()[0], "toolchain-1", "graph-1")
            );
        }
    }
}
