//! Request-owned, byte-pinned compatibility component and exact ancestor capabilities.
use super::*;
use crate::execution::{
    FilesystemAccess, FilesystemGrantKind, GrantResolution, ResolvedGrant,
    RuntimeFilesystemAdditions, resolve_policy,
};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    sync::Arc,
};

#[cfg(test)]
#[allow(dead_code)] // Compile/lint the separately built component in the owning crate's test lane.
#[path = "windows_compat/dll.rs"]
mod component_tests;
#[cfg(test)]
#[path = "windows_compat/imports.rs"]
mod import_tests;
#[allow(dead_code)] // Shared native ABI also serves the separately compiled contained component.
#[path = "windows_compat/native.rs"]
mod native;
#[path = "windows_compat/protocol.rs"]
mod protocol;

const COMPONENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tapid-node-compat.dll"));

#[derive(Debug)]
struct Metadata {
    path: PathBuf,
    file: fs::File,
    identity: (u32, u64),
}

#[derive(Debug)]
pub(in crate::execution) struct Binding {
    directory: PathBuf,
    path: PathBuf,
    pinned: Option<fs::File>,
    metadata: Vec<Metadata>,
}

impl PartialEq for Binding {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.metadata.len() == other.metadata.len()
            && self
                .metadata
                .iter()
                .zip(&other.metadata)
                .all(|(a, b)| a.path == b.path && a.identity == b.identity)
    }
}
impl Eq for Binding {}

fn failure(stage: &str, error: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::UnsupportedContainment,
        format!("Windows standard-Node compatibility {stage} failed: {error}"),
    )
}

impl Binding {
    fn create(request: &ExecutionRequest) -> Result<Self, ExecutionError> {
        if COMPONENT.is_empty() {
            return Err(failure(
                "architecture",
                "component is available only for Windows x64/arm64",
            ));
        }
        let policy = resolve_policy(request, RuntimeFilesystemAdditions::default())?;
        let parent = fs::canonicalize(std::env::temp_dir())
            .map_err(|e| failure("temporary directory", e))?;
        if policy
            .write
            .iter()
            .any(|grant| parent.starts_with(&grant.path))
        {
            return Err(failure(
                "component location",
                "temporary directory is under project write authority",
            ));
        }
        let mut guid = [0u32; 4];
        #[link(name = "ole32")]
        unsafe extern "system" {
            fn CoCreateGuid(value: *mut u32) -> i32;
        }
        if unsafe { CoCreateGuid(guid.as_mut_ptr()) } != 0 {
            return Err(failure("unique component identity", "CoCreateGuid"));
        }
        let directory = parent.join(format!(
            "tapid-node-compat-{:08x}{:08x}{:08x}{:08x}",
            guid[0], guid[1], guid[2], guid[3]
        ));
        fs::create_dir(&directory).map_err(|e| failure("component directory", e))?;
        let directory = fs::canonicalize(directory).map_err(|e| failure("component path", e))?;
        let path = directory.join("tapid-node-compat.dll");
        let mut binding = Self {
            directory,
            path,
            pinned: None,
            metadata: Vec::new(),
        };
        // Toolhelp's module path field is MAX_PATH. Reject instead of resolving a component
        // by basename alone when that field could truncate its identity.
        if wide(binding.path.as_os_str()).len() >= 260 {
            return Err(failure(
                "component path",
                "path exceeds native module identity bound",
            ));
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&binding.path)
            .map_err(|e| failure("component creation", e))?;
        output
            .write_all(COMPONENT)
            .map_err(|e| failure("component write", e))?;
        output
            .sync_all()
            .map_err(|e| failure("component flush", e))?;
        drop(output);
        // Deny replacement/deletion while any owned process may load this component.
        let mut pinned = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&binding.path)
            .map_err(|e| failure("component pin", e))?;
        let mut bytes = Vec::new();
        pinned
            .read_to_end(&mut bytes)
            .map_err(|e| failure("component verification", e))?;
        if bytes != COMPONENT {
            return Err(failure("component verification", "byte identity changed"));
        }
        binding.pinned = Some(pinned);
        let mut paths = BTreeSet::new();
        for grant in policy.read.iter().chain(&policy.write) {
            // Parent metadata never implies permission to enumerate/read that parent.
            for ancestor in grant.path.parent().into_iter().flat_map(Path::ancestors) {
                paths.insert(ancestor.to_path_buf());
            }
        }
        for ancestor in binding.directory.ancestors() {
            paths.insert(ancestor.to_path_buf());
        }
        if paths.len() > protocol::MAX_ANCESTORS {
            return Err(failure("metadata plan", "too many ancestors"));
        }
        for path in paths {
            let units = wide(path.as_os_str());
            if units.len() > protocol::MAX_PATH_UNITS {
                return Err(failure("metadata path", "path exceeds capability bound"));
            }
            let plain = units.strip_prefix(&[92, 92, 63, 92]).unwrap_or(&units);
            if plain.len() < 4
                || !((65..=90).contains(&plain[0]) || (97..=122).contains(&plain[0]))
                || plain[1..3] != [58, 92]
            {
                return Err(failure(
                    "metadata path",
                    "only local drive paths are supported",
                ));
            }
            // Hold exact ordinary objects, including roots, using metadata rights only.
            let raw = unsafe {
                CreateFileW(
                    units.as_ptr(),
                    protocol::METADATA_ACCESS,
                    7,
                    std::ptr::null(),
                    3,
                    0x02200000,
                    0,
                )
            };
            if raw == -1 {
                return Err(failure("metadata open", std::io::Error::last_os_error()));
            }
            let file = unsafe { fs::File::from_raw_handle(raw as *mut std::ffi::c_void) };
            let metadata = file
                .metadata()
                .map_err(|e| failure("metadata inspection", e))?;
            use std::os::windows::fs::MetadataExt;
            if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
                return Err(failure(
                    "metadata topology",
                    "reparse or non-directory ancestor",
                ));
            }
            let identity = crate::execution::request::windows_handle_identity(raw)?;
            binding.metadata.push(Metadata {
                path,
                file,
                identity,
            });
        }
        Ok(binding)
    }

    pub(super) fn additions(&self) -> Result<RuntimeFilesystemAdditions, ExecutionError> {
        let mut additions =
            RuntimeFilesystemAdditions::checked(vec![self.directory.clone()], Vec::new())?;
        for entry in &self.metadata {
            additions.read.push(ResolvedGrant {
                path: entry.path.clone(),
                access: FilesystemAccess::ReadMetadata,
                kind: FilesystemGrantKind::ExactDirectory,
                source: crate::execution::FilesystemGrantSource::BackendRuntime,
                resolution: GrantResolution::RuntimeCanonical,
            });
        }
        Ok(additions)
    }

    pub(super) fn install(&self, process: isize) -> Result<(), ExecutionError> {
        use native::*;
        let path = wide(self.path.as_os_str());
        let (base, size) = load_component(process, &path[..path.len() - 1])
            .map_err(|e| failure("DLL loading", e))?;
        // DONT_RESOLVE_DLL_REFERENCES reads the pinned image's exports without running host code.
        let local = unsafe { LoadLibraryExW(path.as_ptr(), 0, 1) };
        if local == 0 {
            return Err(failure(
                "export inspection",
                std::io::Error::last_os_error(),
            ));
        }
        let export = export_offset(local, c"TapidInitialize", size);
        unsafe { FreeLibrary(local) };
        let export = export.map_err(|e| failure("initialization export", e))?;
        let mut setup = protocol::Setup::empty();
        setup.version = protocol::VERSION;
        setup.count = self.metadata.len() as u32;
        for (source, target) in self.metadata.iter().zip(&mut setup.entries) {
            let identity = crate::execution::request::windows_handle_identity(
                source.file.as_raw_handle() as isize,
            )?;
            if identity != source.identity
                || fs::canonicalize(&source.path)
                    .map_err(|e| failure("metadata revalidation", e))?
                    != source.path
            {
                return Err(failure(
                    "metadata revalidation",
                    "ancestor identity changed",
                ));
            }
            let units = wide(source.path.as_os_str());
            target.length = (units.len() - 1) as u32;
            target.path[..units.len()].copy_from_slice(&units);
            if unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    source.file.as_raw_handle() as isize,
                    process,
                    &mut target.handle,
                    protocol::METADATA_ACCESS,
                    0,
                    0,
                )
            } == 0
            {
                return Err(failure(
                    "metadata delegation",
                    std::io::Error::last_os_error(),
                ));
            }
        }
        let payload = unsafe {
            std::slice::from_raw_parts(
                (&setup as *const protocol::Setup).cast(),
                std::mem::size_of::<protocol::Setup>(),
            )
        };
        if !setup.valid() {
            return Err(failure("metadata setup", "invalid bounded capability plan"));
        }
        if remote_call(process, base + export, payload).map_err(|e| failure("initialization", e))?
            != 1
        {
            return Err(failure("initialization", "component rejected startup"));
        }
        Ok(())
    }
}

pub(super) fn describe() -> String {
    use sha2::{Digest, Sha256};
    format!(
        "Node DLL sha256:{:x}; suspended-child propagation; exact ancestor handles (READ_ATTRIBUTES, non-inherited); EOF stdin; NT realpath; no ancestor ACL writes",
        Sha256::digest(COMPONENT)
    )
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        path: *const u16,
        access: u32,
        share: u32,
        attributes: *const std::ffi::c_void,
        creation: u32,
        flags: u32,
        template: isize,
    ) -> isize;
}

impl Drop for Binding {
    fn drop(&mut self) {
        self.pinned.take();
        // The request owns this component until its Job is confirmed empty. Remove only
        // the exact owned file/directory; no recursive operation touches caller paths.
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.directory);
    }
}

pub(in crate::execution) fn execute(
    request: &ExecutionRequest,
) -> Result<ExecutionOutcome, ExecutionError> {
    request.validate()?;
    if request.policy().mode() == crate::config::SandboxMode::Disabled {
        return crate::execution::execute_with_backend(request, &PlatformBackend);
    }
    crate::execution::supervision::validate_supported_evidence(
        request,
        &containment_support(request),
    )?;
    if request.trusted_node_runtime.is_none() {
        return crate::execution::execute_with_backend(request, &PlatformBackend);
    }
    let mut request = request.clone();
    request.windows_compatibility = Some(Arc::new(Binding::create(&request)?));
    crate::execution::execute_with_backend(&request, &PlatformBackend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AssuranceLevel, FilesystemPolicy, SandboxMode, SandboxPolicy};
    use crate::execution::windows_acl::{initialize_inheritance, read_acl};

    #[test]
    fn failed_component_initialization_never_runs_target_and_revokes_grants() {
        let project = tapid_test_support::TempProject::new("compat-startup-failure").unwrap();
        let runtime = tapid_test_support::TempProject::new("compat-invalid-runtime").unwrap();
        let root = fs::canonicalize(project.path()).unwrap();
        let node = runtime.path().join("node.exe");
        fs::copy(
            PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32")
                .join("cmd.exe"),
            &node,
        )
        .unwrap();
        let node = fs::canonicalize(node).unwrap();
        initialize_inheritance(&root);
        initialize_inheritance(node.parent().unwrap());
        let before = read_acl(&root);
        let policy = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::ManagedTree,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![],
            true,
            ExecutionLimits::new(Some(5), Some(4096), Some(4), None).unwrap(),
        )
        .unwrap();
        let mut request = ExecutionRequest::builder(node.as_os_str())
            .args(["/d", "/c", "echo PROJECT_STARTED"])
            .project_root(&root)
            .policy(policy)
            .executable_search_path(node.parent().unwrap())
            .trusted_node_runtime(&node)
            .build()
            .unwrap();
        let binding = Arc::new(Binding::create(&request).unwrap());
        let component = binding.path.clone();
        let directory = binding.directory.clone();
        // A held read-only image prevents replacement even by a host writer. The
        // project's AppContainer receives only the explicit runtime read/execute grant.
        assert!(fs::OpenOptions::new().write(true).open(&component).is_err());
        request.windows_compatibility = Some(binding);
        let error = crate::execution::execute_with_backend(&request, &PlatformBackend).unwrap_err();
        assert_eq!(
            error.category(),
            ExecutionErrorCategory::UnsupportedContainment
        );
        assert!(
            error.to_string().contains("component rejected startup"),
            "{error}"
        );
        assert_eq!(read_acl(&root), before);
        drop(request);
        assert!(!component.exists());
        assert!(!directory.exists());
    }
}
