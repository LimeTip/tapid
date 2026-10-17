//! Bounded smart HTTP v0 exchange. Git never receives network write authority.
use super::GitDependency;
use crate::transport::{
    STANDARD_ARTIFACT_MAX_RESPONSE_BYTES, STANDARD_METADATA_MAX_RESPONSE_BYTES, read_bounded,
};
use std::{collections::BTreeMap, time::Duration};

pub(super) struct FetchedPack {
    pub object: String,
    pub format: &'static str,
    pub bytes: Vec<u8>,
}

pub(super) fn fetch(spec: &GitDependency) -> Result<FetchedPack, String> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| "cannot initialize Git HTTPS transport")?;
    let base = spec.repository().as_str().trim_end_matches('/');
    fetch_with(
        spec,
        STANDARD_ARTIFACT_MAX_RESPONSE_BYTES,
        |service, body, limit| {
            let request = match body {
                Some(body) => client
                    .post(format!("{base}/{service}"))
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-git-upload-pack-request",
                    )
                    .body(body.to_vec()),
                None => client.get(format!("{base}/{service}")),
            };
            let expected = if body.is_some() {
                "application/x-git-upload-pack-result"
            } else {
                "application/x-git-upload-pack-advertisement"
            };
            let response = request
                .header(reqwest::header::ACCEPT, expected)
                .send()
                .map_err(|_| "Git HTTPS request failed")?;
            if response.status() != reqwest::StatusCode::OK
                || response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    != Some(expected)
            {
                return Err("Git smart HTTPS repository or pinned commit is unavailable".into());
            }
            read_bounded(response, limit)
                .map_err(|_| "Git HTTPS response exceeds its limit or cannot be read".into())
        },
    )
}

// The request seam returns bounded bodies, just like the registry transport seam.
pub(super) fn fetch_with(
    spec: &GitDependency,
    pack_limit: usize,
    mut request: impl FnMut(&str, Option<&[u8]>, usize) -> Result<Vec<u8>, String>,
) -> Result<FetchedPack, String> {
    let advertisement = request(
        "info/refs?service=git-upload-pack",
        None,
        STANDARD_METADATA_MAX_RESPONSE_BYTES,
    )?;
    if advertisement.len() > STANDARD_METADATA_MAX_RESPONSE_BYTES {
        return Err("Git advertisement exceeds 32 MiB".into());
    }
    let mut remaining = advertisement.as_slice();
    if packet(&mut remaining)? != Some(b"# service=git-upload-pack\n".as_slice())
        || packet(&mut remaining)?.is_some()
    {
        return Err("invalid Git service advertisement".into());
    }
    let first = packet(&mut remaining)?.ok_or("Git repository advertises no references")?;
    let first = std::str::from_utf8(first).map_err(|_| "invalid Git advertisement")?;
    let (first_ref, capabilities) = first
        .trim_end_matches('\n')
        .split_once('\0')
        .ok_or("Git capabilities are missing")?;
    let caps = capabilities.split_whitespace().collect::<Vec<_>>();
    let format = match caps
        .iter()
        .find_map(|cap| cap.strip_prefix("object-format="))
    {
        Some("sha256") => "sha256",
        Some("sha1") | None => "sha1",
        _ => return Err("unsupported Git object format".into()),
    };
    if !caps.contains(&"shallow") {
        return Err("Git repository does not support bounded shallow fetches".into());
    }
    let width = if format == "sha256" { 64 } else { 40 };
    let mut refs = BTreeMap::new();
    insert_ref(&mut refs, first_ref, width)?;
    while let Some(line) = packet(&mut remaining)? {
        insert_ref(
            &mut refs,
            std::str::from_utf8(line)
                .map_err(|_| "invalid Git reference")?
                .trim_end_matches('\n'),
            width,
        )?;
    }
    if !remaining.is_empty() {
        return Err("trailing Git advertisement data".into());
    }
    let reference = spec.reference();
    let object = if valid_oid(reference, width) {
        reference.to_ascii_lowercase()
    } else {
        let candidates = [
            reference.to_owned(),
            format!("refs/heads/{reference}"),
            format!("refs/tags/{reference}"),
        ];
        let matches = candidates
            .iter()
            .filter_map(|name| refs.get(name))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err("Git reference is unavailable or ambiguous".into());
        }
        matches[0].clone()
    };
    let mut body = Vec::new();
    let mut requested = format!("want {object} shallow");
    if caps.contains(&"ofs-delta") {
        requested.push_str(" ofs-delta");
    }
    if caps.iter().any(|cap| cap.starts_with("object-format=")) {
        requested.push_str(&format!(" object-format={format}"));
    }
    append_packet(&mut body, &format!("{requested}\n"));
    append_packet(&mut body, "deepen 1\n");
    body.extend_from_slice(b"0000");
    append_packet(&mut body, "done\n");
    let mut bytes = request("git-upload-pack", Some(&body), pack_limit)?;
    if bytes.len() > pack_limit {
        return Err("Git pack response exceeds its limit".into());
    }
    let mut remaining = bytes.as_slice();
    let mut acknowledged = false;
    while !remaining.starts_with(b"PACK") {
        match packet(&mut remaining)? {
            Some(b"NAK\n") => acknowledged = true,
            Some(line) if line.starts_with(b"shallow ") => {
                let oid = std::str::from_utf8(&line[8..])
                    .map_err(|_| "invalid shallow boundary")?
                    .trim_end_matches('\n');
                if !valid_oid(oid, width) {
                    return Err("invalid shallow boundary".into());
                }
            }
            None => {}
            _ => return Err("invalid Git pack response".into()),
        }
    }
    // Bound index/reverse-index disk writes as well as the incoming pack. A pack
    // can claim an enormous object count even when the body is only a few bytes.
    if !acknowledged
        || remaining.len() < 12
        || !matches!(
            u32::from_be_bytes(remaining[4..8].try_into().unwrap()),
            2 | 3
        )
        || u32::from_be_bytes(remaining[8..12].try_into().unwrap()) > 1_000_000
    {
        return Err("Git pack header exceeds supported bounds".into());
    }
    let offset = bytes.len() - remaining.len();
    bytes.drain(..offset);
    Ok(FetchedPack {
        object,
        format,
        bytes,
    })
}

fn valid_oid(value: &str, width: usize) -> bool {
    value.len() == width && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn insert_ref(refs: &mut BTreeMap<String, String>, line: &str, width: usize) -> Result<(), String> {
    let (object, name) = line
        .split_once(' ')
        .ok_or("invalid Git reference advertisement")?;
    if !valid_oid(object, width)
        || name.is_empty()
        || refs
            .insert(name.into(), object.to_ascii_lowercase())
            .is_some()
    {
        return Err("invalid or duplicate Git reference advertisement".into());
    }
    Ok(())
}

fn packet<'a>(input: &mut &'a [u8]) -> Result<Option<&'a [u8]>, String> {
    let header = input.get(..4).ok_or("truncated Git packet")?;
    let length = usize::from_str_radix(
        std::str::from_utf8(header).map_err(|_| "invalid Git packet")?,
        16,
    )
    .map_err(|_| "invalid Git packet length")?;
    if length == 0 {
        *input = &input[4..];
        return Ok(None);
    }
    if !(4..=65520).contains(&length) {
        return Err("unsupported Git packet length".into());
    }
    let payload = input.get(4..length).ok_or("truncated Git packet payload")?;
    *input = &input[length..];
    Ok(Some(payload))
}

fn append_packet(output: &mut Vec<u8>, payload: &str) {
    output.extend_from_slice(format!("{:04x}{payload}", payload.len() + 4).as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{git_command, git_program, run_bounded};
    use std::{fs, process::Stdio};

    fn repository(format: &str) -> (tapid_test_support::TempProject, String) {
        let project = tapid_test_support::TempProject::new("git-smart-http").unwrap();
        let empty = project.write("empty-config", b"").unwrap();
        let program = git_program().unwrap();
        let mut init = git_command(&program, project.path(), &empty);
        init.args(["init", "--template=", "--initial-branch=main"])
            .arg(format!("--object-format={format}"));
        run_bounded(&mut init, 4096).unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"git-fixture","version":"1.0.0"}"#,
            )
            .unwrap();
        project.write("large.txt", &vec![b'x'; 64 * 1024]).unwrap();
        let mut add = git_command(&program, project.path(), &empty);
        add.args(["add", "package.json", "large.txt"]);
        run_bounded(&mut add, 4096).unwrap();
        let mut commit = git_command(&program, project.path(), &empty);
        commit.args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-m",
            "fixture",
        ]);
        run_bounded(&mut commit, 4096).unwrap();
        project
            .write(
                "package.json",
                br#"{"name":"git-fixture","version":"2.0.0"}"#,
            )
            .unwrap();
        let mut commit = git_command(&program, project.path(), &empty);
        commit.args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-am",
            "second fixture",
        ]);
        run_bounded(&mut commit, 4096).unwrap();
        let mut tag = git_command(&program, project.path(), &empty);
        tag.args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "tag",
            "-a",
            "release",
            "-m",
            "release",
        ]);
        run_bounded(&mut tag, 4096).unwrap();
        let mut rev = git_command(&program, project.path(), &empty);
        rev.args(["rev-parse", "HEAD"]);
        let oid = String::from_utf8(run_bounded(&mut rev, 4096).unwrap())
            .unwrap()
            .trim()
            .to_owned();
        (project, oid)
    }

    fn response(project: &tapid_test_support::TempProject, body: Option<&[u8]>) -> Vec<u8> {
        let empty = project.path().join("empty-config");
        let mut command = git_command(&git_program().unwrap(), project.path(), &empty);
        command.args(["upload-pack", "--stateless-rpc"]);
        if let Some(body) = body {
            let input = project.write("request", body).unwrap();
            command.stdin(Stdio::from(fs::File::open(input).unwrap()));
        } else {
            command.arg("--advertise-refs");
        }
        command.arg(project.path());
        let output = run_bounded(&mut command, 1024 * 1024).unwrap();
        if body.is_some() {
            output
        } else {
            let mut advertisement = Vec::new();
            append_packet(&mut advertisement, "# service=git-upload-pack\n");
            advertisement.extend_from_slice(b"0000");
            advertisement.extend_from_slice(&output);
            advertisement
        }
    }

    #[test]
    fn real_smart_http_packs_archive_sha1_and_sha256_repositories() {
        for format in ["sha1", "sha256"] {
            let (remote, oid) = repository(format);
            for reference in ["main", "HEAD", "release", "refs/tags/release", oid.as_str()] {
                let spec =
                    GitDependency::parse(&format!("git+https://example.test/repo#{reference}"))
                        .unwrap();
                let pack = fetch_with(&spec, 1024 * 1024, |_, body, limit| {
                    read_bounded(std::io::Cursor::new(response(&remote, body)), limit)
                        .map_err(|e| e.to_string())
                })
                .unwrap();
                assert_eq!(pack.format, format);
                let scratch =
                    tapid_test_support::TempProject::new("git-smart-http-client").unwrap();
                let (commit, archive) = spec.fetch_with(scratch.path(), pack, run_bounded).unwrap();
                assert_eq!(commit, oid);
                assert!(
                    archive
                        .windows(b"package/package.json".len())
                        .any(|w| w == b"package/package.json")
                );
                assert!(!scratch.path().join("artifact.tar").exists());
                assert!(!scratch.path().join("artifact.pack").exists());
            }
        }
    }

    #[test]
    fn oversized_repository_is_rejected_before_pack_writes_and_archive_spooling() {
        let (remote, _) = repository("sha1");
        let spec = GitDependency::parse("git+https://example.test/repo#main").unwrap();
        let scratch = tapid_test_support::TempProject::new("git-oversized").unwrap();
        let pack = fetch_with(&spec, 16, |_, body, limit| {
            read_bounded(std::io::Cursor::new(response(&remote, body)), limit)
                .map_err(|e| e.to_string())
        });
        assert!(pack.is_err());
        assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
        let pack =
            fetch_with(&spec, 1024 * 1024, |_, body, _| Ok(response(&remote, body))).unwrap();
        let error = spec
            .fetch_with(scratch.path(), pack, |command, limit| {
                let archive = command.get_args().any(|arg| arg == "archive");
                run_bounded(command, if archive { 1024 } else { limit })
            })
            .unwrap_err();
        assert!(error.contains("exceeds its limit"), "{error}");
        assert!(!scratch.path().join("artifact.tar").exists());
    }

    #[test]
    fn malformed_advertisements_and_pack_headers_fail_closed() {
        let (remote, _) = repository("sha1");
        let spec = GitDependency::parse("git+https://example.test/repo#main").unwrap();
        for malformed in [b"0001".to_vec(), b"ffffshort".to_vec(), b"0000".to_vec()] {
            assert!(fetch_with(&spec, 4096, |_, _, _| Ok(malformed.clone())).is_err());
        }
        let mut response = b"0008NAK\nPACK".to_vec();
        response.extend_from_slice(&2_u32.to_be_bytes());
        response.extend_from_slice(&1_000_001_u32.to_be_bytes());
        assert!(
            fetch_with(&spec, 4096, |_, body, _| if body.is_none() {
                Ok(self::response(&remote, None))
            } else {
                Ok(response.clone())
            })
            .is_err()
        );
    }
}
