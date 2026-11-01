//! Retained observations, not a claim of complete loader enforcement.

use super::{ExecutionError, PathBuf, Sha256, frame, rejected};
use std::{fs, io::Read, sync::Arc};

const MAX_CANDIDATES: usize = 64;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RuntimeInputs {
    configuration: String,
    candidates: Vec<Candidate>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Candidate {
    source: PathBuf,
    snapshot: Option<(PathBuf, Arc<[u8]>)>,
}

impl RuntimeInputs {
    pub(super) fn inspect(paths: &[PathBuf], configuration: &str) -> Result<Self, ExecutionError> {
        if paths.len() > MAX_CANDIDATES || configuration.len() > 4096 {
            return Err(rejected("runtime input inventory exceeds bounds"));
        }
        let mut budget = MAX_BYTES;
        let mut candidates = Vec::with_capacity(paths.len());
        for path in paths {
            if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > 4096 {
                return Err(rejected(
                    "runtime candidate must be a bounded absolute path",
                ));
            }
            candidates.push(Candidate::inspect(path, &mut budget)?);
        }
        Ok(Self {
            configuration: configuration.into(),
            candidates,
        })
    }

    pub(super) fn validate(&self) -> Result<(), ExecutionError> {
        let mut budget = MAX_BYTES;
        for candidate in &self.candidates {
            if Candidate::inspect(&candidate.source, &mut budget)? != *candidate {
                return Err(rejected("prepared runtime candidate changed"));
            }
        }
        Ok(())
    }

    pub(super) fn hash_into(&self, hash: &mut Sha256) {
        frame(hash, b"ordered-runtime-observations-v1");
        frame(hash, self.configuration.as_bytes());
        for candidate in &self.candidates {
            frame(hash, candidate.source.as_os_str().as_encoded_bytes());
            match &candidate.snapshot {
                None => frame(hash, b"absent"),
                Some((canonical, bytes)) => {
                    frame(hash, b"present");
                    frame(hash, canonical.as_os_str().as_encoded_bytes());
                    frame(hash, bytes);
                }
            }
        }
    }
}

impl Candidate {
    fn inspect(source: &PathBuf, budget: &mut usize) -> Result<Self, ExecutionError> {
        let metadata = match fs::symlink_metadata(source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Windows can report PATH_NOT_FOUND for a regular-file ancestor.
                // Only a missing leaf under an inspectable directory is absence.
                if !source.parent().is_some_and(|parent| {
                    fs::metadata(parent).is_ok_and(|metadata| metadata.is_dir())
                }) {
                    return Err(rejected("cannot establish runtime candidate absence"));
                }
                return Ok(Self {
                    source: source.clone(),
                    snapshot: None,
                });
            }
            Err(_) => return Err(rejected("cannot inspect runtime candidate")),
        };
        if !(metadata.is_file() || metadata.file_type().is_symlink())
            || (metadata.is_file() && metadata.len() > *budget as u64)
        {
            return Err(rejected("runtime candidate is not a bounded regular file"));
        }
        let canonical =
            fs::canonicalize(source).map_err(|_| rejected("cannot resolve runtime candidate"))?;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ);
        }
        let file = options
            .open(&canonical)
            .map_err(|_| rejected("cannot open runtime candidate"))?;
        if !file
            .metadata()
            .map_err(|_| rejected("cannot inspect opened runtime candidate"))?
            .is_file()
        {
            return Err(rejected("opened runtime candidate is not a regular file"));
        }
        let mut bytes = Vec::new();
        file.take(*budget as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| rejected("cannot read runtime candidate"))?;
        if bytes.len() > *budget || fs::canonicalize(source).ok().as_ref() != Some(&canonical) {
            return Err(rejected("runtime candidate changed or exceeds bounds"));
        }
        *budget -= bytes.len();
        Ok(Self {
            source: source.clone(),
            snapshot: Some((canonical, bytes.into())),
        })
    }
}
