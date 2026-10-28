use super::*;
use std::sync::Mutex;
use tapid_registry_client::{HttpResponse, TransportError};
use tapid_test_support::TempProject;

const NPM: &str = "https://registry.npmjs.org";

struct CountingTransport {
    requests: Mutex<Vec<String>>,
    reverse: bool,
    fail: bool,
}

impl HttpTransport for CountingTransport {
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(url.to_owned());
        if self.fail {
            return Err(TransportError::Http("fixture failure".into()));
        }
        let name = url.rsplit('/').next().unwrap();
        let mut versions = vec!["1.0.0", "1.9.0", "2.0.0"];
        if self.reverse {
            versions.reverse();
        }
        let records = versions.into_iter().map(|version| format!(
            r#""{version}":{{"name":"{name}","version":"{version}","dist":{{"tarball":"https://cdn.example/package.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#
        )).collect::<Vec<_>>().join(",");
        Ok(HttpResponse {
            status: 200,
            content_type: Some("application/json".into()),
            body: format!(r#"{{"name":"{name}","versions":{{{records}}}}}"#).into_bytes(),
        })
    }
}

fn project() -> TempProject {
    let project = TempProject::new("outdated-cache").unwrap();
    project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"a":"npm:target@^1","b":"npm:target@^2","other":"*"},"devDependencies":{"a":"npm:target@^2"}}"#).unwrap();
    project
        .write(
            "tapid.lock",
            Lockfile::new(&format!("sha256-{}", "a".repeat(64)))
                .unwrap()
                .to_json()
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    project
}

fn transport(reverse: bool, fail: bool) -> CountingTransport {
    CountingTransport {
        requests: Mutex::new(Vec::new()),
        reverse,
        fail,
    }
}

fn run(project: &TempProject, transport: &CountingTransport) -> Vec<OutdatedEntry> {
    outdated_entries_with(
        project.path(),
        None,
        None,
        false,
        &mut OperationOutcome::unchanged(project.path()),
        &mut |_, origin, name| versions_from_registry(transport, origin, name),
        &mut |_| panic!("registry mode must not read fixture"),
    )
    .unwrap()
}

type EntrySummary<'a> = (&'a str, &'a str, &'a str, Option<&'a str>, Option<&'a str>);

fn summary(entries: &[OutdatedEntry]) -> Vec<EntrySummary<'_>> {
    entries
        .iter()
        .map(|entry| {
            (
                entry.identity.as_str(),
                entry.kind.as_str(),
                entry.declared.as_str(),
                entry.newest_compatible.as_deref(),
                entry.newest_available.as_deref(),
            )
        })
        .collect()
}

#[test]
fn outdated_cache_reuses_aliases_and_sections_per_invocation() {
    let project = project();
    let transport = transport(false, false);
    let entries = run(&project, &transport);
    assert_eq!(
        summary(&entries),
        vec![
            (
                "a",
                "dependencies",
                "npm:target@^1",
                Some("1.9.0"),
                Some("2.0.0")
            ),
            (
                "a",
                "devDependencies",
                "npm:target@^2",
                Some("2.0.0"),
                Some("2.0.0")
            ),
            (
                "b",
                "dependencies",
                "npm:target@^2",
                Some("2.0.0"),
                Some("2.0.0")
            ),
            ("other", "dependencies", "*", Some("2.0.0"), Some("2.0.0")),
        ]
    );
    let requests = vec![format!("{NPM}/target"), format!("{NPM}/other")];
    assert_eq!(*transport.requests.lock().unwrap(), requests);
    let again = run(&project, &transport);
    assert_eq!(summary(&entries), summary(&again));
    assert_eq!(
        *transport.requests.lock().unwrap(),
        [requests.clone(), requests].concat()
    );
    let reversed = run(&project, &self::transport(true, false));
    assert_eq!(summary(&entries), summary(&reversed));
}

fn assert_context(entries: &[OutdatedEntry], message: &str, kind: ErrorKind) {
    for entry in entries {
        let error = entry.diagnostic.as_ref().unwrap();
        assert_eq!(error.kind, kind);
        assert!(error.to_string().contains(message), "{error}");
        let context = error.diagnostic.as_ref().unwrap();
        assert_eq!(
            context.declaration.as_ref().unwrap().pointer,
            format!("/{}/{}", entry.kind, entry.identity)
        );
        assert_eq!(
            context.package.as_ref().unwrap().name,
            if entry.identity == "other" {
                "other"
            } else {
                "target"
            }
        );
    }
}

#[test]
fn outdated_cache_reuses_failures_without_caching_declaration_context() {
    let project = project();
    let transport = transport(false, true);
    let entries = run(&project, &transport);
    assert_eq!(transport.requests.lock().unwrap().len(), 2);
    assert_context(&entries, "fixture failure", ErrorKind::RegistryTransport);
    let again = run(&project, &transport);
    assert_eq!(summary(&entries), summary(&again));
    assert_context(&again, "fixture failure", ErrorKind::RegistryTransport);
}

#[test]
fn outdated_cache_isolates_exact_origins_and_cached_errors() {
    for fail_second in [false, true] {
        let mut cache = MetadataCache::new();
        let mut calls = Vec::new();
        for _ in 0..2 {
            for (origin, version) in [(NPM, "1.0.0"), ("https://private.example", "2.0.0")] {
                let value = cached_versions(
                    &mut cache,
                    origin.parse().unwrap(),
                    "target".parse().unwrap(),
                    || {
                        calls.push((origin, "target"));
                        if fail_second && origin != NPM {
                            Err(OperationalError::new(
                                ErrorKind::RegistryMetadata,
                                "private failure",
                            ))
                        } else {
                            Ok(vec![(version.parse().unwrap(), BTreeSet::new())])
                        }
                    },
                );
                if fail_second && origin != NPM {
                    assert_eq!(
                        value.as_ref().unwrap_err(),
                        &(ErrorKind::RegistryMetadata, "private failure".into())
                    );
                } else {
                    assert_eq!(value.as_ref().unwrap()[0].0.to_string(), version);
                }
            }
        }
        assert_eq!(
            calls,
            vec![(NPM, "target"), ("https://private.example", "target")]
        );
    }
}

#[test]
fn outdated_cache_initializes_fixture_once_including_read_and_parse_errors() {
    for (contents, expected) in [
        (None, Some("cannot read registry fixture")),
        (Some("{"), Some("invalid registry fixture")),
        (
            Some(
                r#"{"packages":[{"registry":"https://registry.npmjs.org","name":"target","version":"1.0.0"},{"registry":"https://registry.npmjs.org","name":"other","version":"2.0.0"},{"registry":"https://registry.npmjs.org","name":"unmatched","version":"invalid"}]}"#,
            ),
            None,
        ),
    ] {
        let project = project();
        let path = project.path().join("registry.json");
        if let Some(contents) = contents {
            project.write("registry.json", contents.as_bytes()).unwrap();
        }
        let mut reads = 0;
        let entries = outdated_entries_with(
            project.path(),
            None,
            Some(&path),
            false,
            &mut OperationOutcome::unchanged(project.path()),
            &mut |_, _, _| panic!("fixture mode must not fetch registry"),
            &mut |path| {
                reads += 1;
                read_fixture(path)
            },
        )
        .unwrap();
        assert_eq!(reads, 1);
        assert_eq!(entries.len(), 4);
        if let Some(message) = expected {
            assert_context(&entries, message, ErrorKind::RegistryMetadata);
        } else {
            assert!(entries.iter().all(|entry| entry.diagnostic.is_none()));
            assert_eq!(entries[0].newest_available.as_deref(), Some("1.0.0"));
            assert_eq!(entries[3].newest_available.as_deref(), Some("2.0.0"));
        }
    }
}

#[test]
fn outdated_cache_invalid_matching_fixture_versions_keep_per_declaration_errors() {
    let project = project();
    project.write("registry.json", br#"{"packages":[{"registry":"https://registry.npmjs.org","name":"target","version":"invalid"},{"registry":"https://registry.npmjs.org","name":"other","version":"1.0.0"}]}"#).unwrap();
    let mut reads = 0;
    let entries = outdated_entries_with(
        project.path(),
        None,
        Some(&project.path().join("registry.json")),
        false,
        &mut OperationOutcome::unchanged(project.path()),
        &mut |_, _, _| panic!("unexpected registry fetch"),
        &mut |path| {
            reads += 1;
            read_fixture(path)
        },
    )
    .unwrap();
    assert_eq!(reads, 1);
    assert_context(&entries[..3], "invalid", ErrorKind::RegistryMetadata);
    assert!(entries[3].diagnostic.is_none());
}

#[test]
fn outdated_cache_routes_aliases_by_actual_target() {
    let project = project();
    project.write("package.json", br#"{"name":"app","version":"1.0.0","dependencies":{"@local/alias":"npm:target@^1","public-alias":"npm:@private/target@^2"}}"#).unwrap();
    project
        .write(
            "tapid.toml",
            br#"[registries.'@local']
url='https://wrong.example'
[registries.'@private']
url='https://private.example'
"#,
        )
        .unwrap();
    let mut requests = Vec::new();
    let entries = outdated_entries_with(
        project.path(),
        None,
        None,
        false,
        &mut OperationOutcome::unchanged(project.path()),
        &mut |config, origin, name| {
            assert_eq!(*origin, config.origin_for_name(name).unwrap());
            requests.push((origin.to_string(), name.to_string()));
            Ok(vec![("2.0.0".parse().unwrap(), BTreeSet::new())])
        },
        &mut |_| panic!("unexpected fixture read"),
    )
    .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        requests,
        vec![
            (NPM.into(), "target".into()),
            ("https://private.example".into(), "@private/target".into())
        ]
    );
}
