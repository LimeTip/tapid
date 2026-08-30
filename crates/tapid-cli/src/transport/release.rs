use std::{io, io::Read, process::Stdio};

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
    fn fetch_limited(&mut self, url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        if !url.starts_with("https://") {
            return Err("URL must use HTTPS".into());
        }
        let mut child = std::process::Command::new("curl")
            .args(curl_fetch_args(url, max_bytes))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("HTTPS transport unavailable: {e}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "HTTPS transport unavailable: missing curl output".to_owned())?;
        let bytes = match read_bounded(stdout, max_bytes) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("cannot read HTTPS response: {error}"));
            }
        };
        let status = child
            .wait()
            .map_err(|e| format!("cannot finish HTTPS request: {e}"))?;
        if status.success() {
            Ok(bytes)
        } else {
            Err("HTTPS request failed".into())
        }
    }
}

impl tapid_release_client::Fetcher for CurlFetcher {
    fn fetch(&mut self, url: &str) -> Result<Vec<u8>, String> {
        self.fetch_limited(url, MAX_FETCH_BYTES)
    }

    fn fetch_with_limit(&mut self, url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        self.fetch_limited(url, max_bytes)
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
    use crate::filesystem::tree::{
        cmd_batch_path, cmd_shim_contents, powershell_shim_contents, powershell_single_quoted,
    };

    #[test]
    fn bounded_response_reader_caps_initial_allocation() {
        assert_eq!(bounded_read_capacity(2), 3);
        assert_eq!(bounded_read_capacity(MAX_ARTIFACT_BYTES), 64 * 1024);
        assert!(read_bounded(&b"ok"[..], MAX_ARTIFACT_BYTES).is_ok());
    }

    #[test]
    fn bounded_response_reader_rejects_streams_without_known_size() {
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

    #[test]
    fn powershell_single_quoted_escapes_apostrophes() {
        assert_eq!(
            powershell_single_quoted(r"C:\\Users\O'Brien\project\tapid.exe"),
            r"C:\\Users\O''Brien\project\tapid.exe"
        );
    }

    #[test]
    fn powershell_single_quoted_preserves_other_path_characters() {
        assert_eq!(
            powershell_single_quoted(r"C:\\Program Files\tapid.exe"),
            r"C:\\Program Files\tapid.exe"
        );
    }

    #[test]
    fn windows_shims_resolve_sources_relative_to_the_surviving_bin_directory() {
        let parent = std::path::Path::new("/project/node_modules/.bin");
        let source = std::path::Path::new("/project/node_modules/tool/cli.js");

        let relative = format!(
            "..{}tool{}cli.js",
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        );
        assert_eq!(
            cmd_shim_contents(parent, source),
            format!("@echo off\r\n@setlocal DisableDelayedExpansion\r\n\"%~dp0{relative}\" %*\r\n")
        );
        assert_eq!(
            powershell_shim_contents(parent, source),
            format!("& (Join-Path $PSScriptRoot '{relative}') $args\r\n")
        );
    }

    #[test]
    fn cmd_batch_path_escapes_percent_and_preserves_other_characters() {
        assert_eq!(
            cmd_batch_path(r"C:\\100%\O'Brien\tapid.exe"),
            r"C:\\100%%\O'Brien\tapid.exe"
        );
    }
}
