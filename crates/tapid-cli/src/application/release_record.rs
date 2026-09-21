//! Provider-neutral HTTPS release discovery shared with the bootstrap installers.
use std::collections::BTreeSet;
use tapid_release_client::Error;

pub(super) const DEFAULT_URL: &str = "https://tapid.dev/releases/v1/latest.tsv";
pub(super) const MAX_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub(super) struct ReleaseRecord {
    pub version: String,
    pub name: String,
    pub size: usize,
    pub sha256: String,
    pub url: String,
}

pub(super) fn https_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let (host, valid_port) = match authority.split_once(':') {
        Some((host, port)) => (
            host,
            !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()),
        ),
        None => (authority, true),
    };
    host.as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        && valid_port
        && value.is_ascii()
        && !value.bytes().any(|b| {
            b.is_ascii_control()
                || b.is_ascii_whitespace()
                || matches!(b, b'@' | b'\\' | b'#' | b'?')
        })
}

fn stable_version(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && (part.len() == 1 || !part.starts_with('0'))
                && part.bytes().all(|b| b.is_ascii_digit())
                && part.parse::<u64>().is_ok()
        })
}

pub(super) fn parse(bytes: &[u8], target: &str) -> Result<ReleaseRecord, Error> {
    let invalid = || Error::InvalidManifest("invalid tapid-release-v1 record".into());
    if bytes.len() > MAX_BYTES || !bytes.ends_with(b"\n") || bytes.contains(&b'\r') {
        return Err(invalid());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut lines = text.strip_suffix('\n').ok_or_else(invalid)?.split('\n');
    let header: Vec<_> = lines.next().ok_or_else(invalid)?.split('\t').collect();
    if header.len() != 2 || header[0] != "tapid-release-v1" || !stable_version(header[1]) {
        return Err(invalid());
    }
    let version = header[1];
    let mut seen = BTreeSet::new();
    let mut selected = None;
    for line in lines {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 5 {
            return Err(invalid());
        }
        let [row_target, name, size, sha256, url] = fields[..] else {
            unreachable!()
        };
        if row_target.is_empty()
            || !row_target
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || !seen.insert(row_target)
            || name != format!("tapid-{version}-{row_target}.tar.gz")
            || size.starts_with('0')
            || !size.bytes().all(|b| b.is_ascii_digit())
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !https_url(url)
        {
            return Err(invalid());
        }
        let size: usize = size.parse().map_err(|_| invalid())?;
        if size == 0 || size > crate::filesystem::atomic::MAX_ARTIFACT_BYTES {
            return Err(invalid());
        }
        if row_target == target {
            selected = Some(ReleaseRecord {
                version: version.into(),
                name: name.into(),
                size,
                sha256: sha256.into(),
                url: url.into(),
            });
        }
    }
    selected.ok_or_else(|| Error::TargetNotFound(target.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(url: &str) -> String {
        format!(
            "tapid-release-v1\t1.2.3\nx86_64-test\ttapid-1.2.3-x86_64-test.tar.gz\t42\t{}\t{url}\n",
            "a".repeat(64)
        )
    }

    #[test]
    fn record_is_independent_of_download_provider() {
        for url in [
            "https://github.com/org/repo/releases/download/v1.2.3/file",
            "https://gitlab.example.org/releases/1.2.3/file",
            "https://cdn.example.org/artifacts/file",
        ] {
            let parsed = parse(record(url).as_bytes(), "x86_64-test").unwrap();
            assert_eq!(parsed.version, "1.2.3");
            assert_eq!(parsed.url, url);
            assert_eq!(parsed.size, 42);
        }
    }

    #[test]
    fn rejects_ambiguous_or_unsafe_records() {
        let valid = record("https://example.test/file");
        let row = valid.split_once('\n').unwrap().1;
        for bad in [
            valid.replace("1.2.3", "01.2.3"),
            valid.replace("1.2.3", "1.2.3-beta"),
            valid.replace("\t42\t", "\t0\t"),
            valid.replace("\t42\t", "\t042\t"),
            valid.replace("\t42\t", "\t536870913\t"),
            valid.replace("tapid-1.2.3", "../tapid-1.2.3"),
            valid.replace("https://", "http://"),
            valid.replace("example.test", "user@example.test"),
            valid.replace("/file", "/file?query=1"),
            valid.replace("/file", "/file#fragment"),
            valid.replace("/file", "/file\\bad"),
            valid.replace("/file", "/file\0bad"),
            format!("{valid}{row}"),
            format!("{valid}\n"),
            valid.trim_end().into(),
            valid.replace('\n', "\r\n"),
        ] {
            assert!(parse(bad.as_bytes(), "x86_64-test").is_err(), "{bad:?}");
        }
    }
}
