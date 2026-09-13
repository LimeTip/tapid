use std::{io, io::Read, process::Stdio};
use tapid_release_client::Error as ReleaseError;

const CURL_CONNECT_TIMEOUT_SECONDS: &str = "10";
const CURL_MAX_TIME_SECONDS: &str = "30";
const MAX_FETCH_BYTES: usize = 256 * 1024;
pub(crate) fn bounded_read_capacity(max_bytes: usize) -> usize {
    max_bytes.saturating_add(1).min(64 * 1024)
}

pub(crate) fn read_bounded<R: Read>(reader: R, max_bytes: usize) -> io::Result<Vec<u8>> {
    let read_limit = max_bytes.checked_add(1).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "response limit is too large")
    })?;
    let mut bytes = Vec::with_capacity(bounded_read_capacity(max_bytes));
    reader.take(read_limit as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "response exceeds maximum size",
        ));
    }
    Ok(bytes)
}

pub(crate) fn curl_fetch_args(url: &str, max_bytes: usize) -> Vec<String> {
    let mut args = vec![
        "--fail".to_owned(),
        "--silent".to_owned(),
        "--show-error".to_owned(),
        "--location".to_owned(),
        "--proto".to_owned(),
        "=https".to_owned(),
        "--tlsv1.2".to_owned(),
        "--connect-timeout".to_owned(),
        CURL_CONNECT_TIMEOUT_SECONDS.to_owned(),
        "--max-time".to_owned(),
        CURL_MAX_TIME_SECONDS.to_owned(),
        "--max-filesize".to_owned(),
        max_bytes.to_string(),
    ];
    args.push(url.to_owned());
    args
}

pub(crate) struct CurlFetcher;
impl CurlFetcher {
    fn fetch_limited(&mut self, url: &str, max_bytes: usize) -> Result<Vec<u8>, ReleaseError> {
        if !url.starts_with("https://") {
            return Err(ReleaseError::Fetch("URL must use HTTPS".into()));
        }
        let mut child = std::process::Command::new("curl")
            .args(curl_fetch_args(url, max_bytes))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| ReleaseError::Fetch(format!("HTTPS transport unavailable: {e}")))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ReleaseError::Fetch("HTTPS transport unavailable: missing curl output".into())
        })?;
        let bytes = match read_bounded(stdout, max_bytes) {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(response_read_error(error));
            }
        };
        let status = child
            .wait()
            .map_err(|e| ReleaseError::Fetch(format!("cannot finish HTTPS request: {e}")))?;
        classify_curl_response(status.code(), bytes)
    }
}

impl tapid_release_client::Fetcher for CurlFetcher {
    fn fetch(&mut self, url: &str) -> Result<Vec<u8>, String> {
        self.fetch_limited(url, MAX_FETCH_BYTES)
            .map_err(|error| error.to_string())
    }

    fn fetch_with_limit(&mut self, url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        self.fetch_limited(url, max_bytes)
            .map_err(|error| error.to_string())
    }

    fn fetch_metadata_with_limit(
        &mut self,
        url: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ReleaseError> {
        self.fetch_limited(url, max_bytes)
    }
}

/// A failed bounded read cannot establish that no rejected response was received.
fn response_read_error(error: io::Error) -> ReleaseError {
    ReleaseError::InvalidManifest(format!(
        "cannot read complete bounded HTTPS response: {error}"
    ))
}

/// Distinguish absent responses from rejected or partially received metadata.
fn classify_curl_response(code: Option<i32>, bytes: Vec<u8>) -> Result<Vec<u8>, ReleaseError> {
    if code == Some(0) {
        Ok(bytes)
    } else if matches!(code, Some(18 | 63)) || !bytes.is_empty() {
        // curl exits 18 (CURLE_PARTIAL_FILE) and 63 (CURLE_FILESIZE_EXCEEDED)
        // reject received responses even when no body bytes were emitted.
        Err(ReleaseError::InvalidManifest(
            "HTTPS response exceeded its size limit or was incomplete".into(),
        ))
    } else {
        Err(ReleaseError::Fetch("HTTPS request failed".into()))
    }
}

pub(crate) fn release_target() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unsupported-target"
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CURL_CONNECT_TIMEOUT_SECONDS, CURL_MAX_TIME_SECONDS, bounded_read_capacity,
        curl_fetch_args, read_bounded,
    };
    use crate::filesystem::atomic::MAX_ARTIFACT_BYTES;

    #[test]
    fn oversized_response_read_is_not_a_transport_outage() {
        let error = read_bounded(&b"too large"[..], 2).unwrap_err();
        assert!(matches!(
            super::response_read_error(error),
            super::ReleaseError::InvalidManifest(_)
        ));
    }

    #[test]
    fn curl_size_limit_exit_is_not_a_transport_outage() {
        assert!(matches!(
            super::classify_curl_response(Some(63), vec![]),
            Err(super::ReleaseError::InvalidManifest(_))
        ));
    }

    #[test]
    fn partial_curl_response_is_not_a_transport_outage() {
        for bytes in [vec![], b"partial".to_vec()] {
            assert!(matches!(
                super::classify_curl_response(Some(18), bytes),
                Err(super::ReleaseError::InvalidManifest(_))
            ));
        }
    }

    #[test]
    fn curl_unavailability_remains_retryable() {
        for code in [6, 7, 22, 28, 35, 60] {
            assert!(matches!(
                super::classify_curl_response(Some(code), vec![]),
                Err(super::ReleaseError::Fetch(_))
            ));
        }
        assert_eq!(
            super::classify_curl_response(Some(0), b"ok".to_vec()).unwrap(),
            b"ok"
        );
    }

    #[test]
    fn bounded_response_reader_caps_initial_allocation() {
        assert_eq!(bounded_read_capacity(2), 3);
        assert_eq!(bounded_read_capacity(MAX_ARTIFACT_BYTES), 64 * 1024);
        assert!(read_bounded(&b"ok"[..], MAX_ARTIFACT_BYTES).is_ok());
    }

    #[test]
    fn bounded_response_reader_rejects_streams_over_the_size_limit() {
        assert!(read_bounded(&b"ok"[..], 2).is_ok());
        assert!(read_bounded(&b"too large"[..], 2).is_err());
    }

    #[test]
    fn release_fetches_have_bounded_connection_and_total_time() {
        let args = curl_fetch_args("https://example.test/stable.json", 256 * 1024);
        assert_eq!(
            args,
            vec![
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
                "--tlsv1.2",
                "--connect-timeout",
                CURL_CONNECT_TIMEOUT_SECONDS,
                "--max-time",
                CURL_MAX_TIME_SECONDS,
                "--max-filesize",
                "262144",
                "https://example.test/stable.json",
            ]
        );
    }

    #[test]
    fn artifact_fetches_use_the_declared_size_limit() {
        let args = curl_fetch_args("https://example.test/tapid.tar.gz", 1024 * 1024);
        assert_eq!(args[args.len() - 2], "1048576");
        assert_eq!(args.last().unwrap(), "https://example.test/tapid.tar.gz");
    }
}
