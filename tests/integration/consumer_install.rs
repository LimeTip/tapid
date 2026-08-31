use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use std::sync::Arc;
use std::thread;

use sha2::{Digest, Sha256};
use tapid_archive::{ArchiveFormat, ArchiveLimits, canonical_tree_digest, extract_to};
use tapid_core::{ArtifactDigest, RegistryOrigin};
use tapid_lockfile::{LockedPackage, Lockfile};
use tapid_registry_client::{
    FetchMode, HttpResponse, HttpTransport, JsrRegistry, NpmRegistry, RegistryClient,
    RegistryTransport, TransportError,
};
use tapid_resolver::{
    Dependency, PackageVersionMetadata, RegistryMetadata, Requirement, ResolutionOptions,
    resolve_graph,
};
use tapid_store::{IngestResult, Store};
use tapid_test_support::TempProject;

#[derive(Clone)]
struct LocalHttpFixture {
    address: std::net::SocketAddr,
}

impl LocalHttpFixture {
    fn start(routes: BTreeMap<String, (u16, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let routes = Arc::new(routes);
        let server_routes = Arc::clone(&routes);
        thread::spawn(move || {
            for stream in listener.incoming().take(16) {
                if let Ok(mut stream) = stream {
                    serve_once(&mut stream, &server_routes);
                }
            }
        });
        Self { address }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, path)
    }
}

fn serve_once(stream: &mut TcpStream, routes: &BTreeMap<String, (u16, Vec<u8>)>) {
    let mut request = [0u8; 4096];
    let size = stream.read(&mut request).unwrap_or(0);
    let request = String::from_utf8_lossy(&request[..size]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let (status, body) = routes
        .get(path)
        .cloned()
        .unwrap_or((404, b"not found".to_vec()));
    let reason = if status == 200 { "OK" } else { "Found" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(&body);
}

#[derive(Clone)]
struct LocalTransport {
    fixture: LocalHttpFixture,
    max_response_bytes: usize,
}

impl LocalTransport {
    fn get_http(&self, url: &str) -> Result<HttpResponse, TransportError> {
        let parsed = url::Url::parse(url).map_err(|_| TransportError::InvalidUrl(url.into()))?;
        let allowed = [
            "https://registry.npmjs.org",
            "https://jsr.io",
            "https://npm.jsr.io",
        ];
        let origin = format!(
            "{}://{}",
            parsed.scheme(),
            parsed.host_str().unwrap_or_default()
        );
        if !allowed.contains(&origin.as_str()) {
            return Err(TransportError::OriginNotAllowed(url.into()));
        }
        let mut stream = TcpStream::connect(self.fixture.address)
            .map_err(|e| TransportError::Http(e.to_string()))?;
        let path = parsed.path();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: fixture\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .map_err(|e| TransportError::Http(e.to_string()))?;
        let mut bytes = Vec::new();
        stream
            .read_to_end(&mut bytes)
            .map_err(|e| TransportError::Http(e.to_string()))?;
        let split = bytes
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| TransportError::InvalidResponse("missing HTTP headers".into()))?;
        let headers = String::from_utf8_lossy(&bytes[..split]);
        let status = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| TransportError::InvalidResponse("invalid HTTP status".into()))?;
        let body = bytes[split + 4..].to_vec();
        if (300..400).contains(&status) {
            return Err(TransportError::InvalidResponse("redirect rejected".into()));
        }
        if body.len() > self.max_response_bytes {
            return Err(TransportError::TooLarge {
                limit: self.max_response_bytes,
            });
        }
        Ok(HttpResponse {
            status,
            content_type: Some("application/json".into()),
            body,
        })
    }
}

impl HttpTransport for LocalTransport {
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
        self.get_http(url)
    }
}

struct SnapshotTransport;
impl RegistryTransport for SnapshotTransport {
    type Error = std::io::Error;
    fn fetch(
        &self,
        _: &RegistryOrigin,
    ) -> Result<tapid_registry_client::RawRegistrySnapshot, Self::Error> {
        panic!("offline and frozen replay must not invoke transport")
    }
}

fn sha256_digest(bytes: &[u8]) -> ArtifactDigest {
    let digest = Sha256::digest(bytes);
    format!("sha256-{digest:x}").parse().unwrap()
}

fn tar_fixture() -> Vec<u8> {
    let data = b"export const answer = 42;\n";
    let name = b"package/index.js";
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name);
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    header[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    for byte in &mut header[148..156] {
        *byte = b' ';
    }
    let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
    header[148..156].copy_from_slice(format!("{:06o}\0 ", checksum).as_bytes());
    let mut archive = Vec::from(header);
    archive.extend_from_slice(data);
    archive.resize(1024, 0);
    archive
}

#[test]
fn local_npm_and_jsr_contracts_cover_replay_and_security_boundaries() {
    let archive = tar_fixture();
    let npm_meta = br#"{"name":"demo","versions":{"1.0.0":{"name":"demo","version":"1.0.0","dist":{"tarball":"https://registry.npmjs.org/demo/-/demo-1.0.0.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}}}}"#.to_vec();
    let jsr_meta = br#"{"scope":"std","name":"path","latest":"1.0.0","versions":{"1.0.0":{"npm":{"tarball":"https://npm.jsr.io/~/std__path/1.0.0.tgz","integrity":"sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="}}}}"#.to_vec();
    let fixture = LocalHttpFixture::start(BTreeMap::from([
        ("/demo".into(), (200, npm_meta)),
        ("/@std/path/meta.json".into(), (200, jsr_meta)),
        ("/demo/-/demo-1.0.0.tgz".into(), (200, archive.clone())),
        ("/~/std__path/1.0.0.tgz".into(), (200, archive.clone())),
        ("/redirect".into(), (302, b"/demo".to_vec())),
        ("/large".into(), (200, vec![b'x'; 32])),
    ]));
    let transport = LocalTransport {
        fixture: fixture.clone(),
        max_response_bytes: 1024 * 1024,
    };
    let npm_origin: RegistryOrigin = "https://registry.npmjs.org".parse().unwrap();
    let jsr_origin: RegistryOrigin = "https://jsr.io".parse().unwrap();
    let npm = NpmRegistry::new(transport.clone(), npm_origin.clone())
        .fetch("demo")
        .unwrap();
    let jsr = JsrRegistry::new(transport.clone(), jsr_origin.clone())
        .fetch("@std/path")
        .unwrap();
    assert_eq!(npm[0].identity.version.to_string(), "1.0.0");
    assert!(npm[0].integrity.is_some());
    assert_eq!(
        jsr[0].artifact_url,
        "https://npm.jsr.io/~/std__path/1.0.0.tgz"
    );
    assert!(matches!(
        transport.get(&format!("https://registry.npmjs.org/redirect")),
        Err(TransportError::InvalidResponse(_))
    ));
    let bounded = LocalTransport {
        fixture: fixture.clone(),
        max_response_bytes: 8,
    };
    assert_eq!(
        bounded.get("https://registry.npmjs.org/large"),
        Err(TransportError::TooLarge { limit: 8 })
    );

    let project = TempProject::new("consumer-install").unwrap();
    let store = Store::new(project.path().join("store"));
    let archive_digest = sha256_digest(&archive);
    let unpack_root = project.path().join("unpack");
    extract_to(
        &archive,
        ArchiveFormat::Tar,
        &unpack_root,
        ArchiveLimits::default(),
    )
    .unwrap();
    let tree_digest: ArtifactDigest = canonical_tree_digest(&unpack_root)
        .unwrap()
        .parse()
        .unwrap();
    let activated = store
        .ingest_archive(
            &archive,
            &archive_digest,
            &tree_digest,
            ArchiveFormat::Tar,
            ArchiveLimits::default(),
        )
        .unwrap();
    assert!(matches!(activated, IngestResult::Activated(_)));
    assert_eq!(
        std::fs::read_to_string(
            store
                .verified_tree_path(&tree_digest)
                .unwrap()
                .join(".tapid-tree")
        )
        .unwrap(),
        tree_digest.to_string()
    );
    assert!(matches!(
        store.ingest(&archive_digest, &archive[..archive.len() - 1]),
        Err(tapid_store::IngestError::DigestMismatch { .. })
    ));

    let dependency = Dependency::new(
        npm_origin.clone(),
        "demo".parse().unwrap(),
        "1.0.0".parse::<Requirement>().unwrap(),
    );
    let transitive = RegistryMetadata::normalize(
        npm_origin.clone(),
        vec![
            PackageVersionMetadata {
                name: "demo".parse().unwrap(),
                version: "1.0.0".parse().unwrap(),
                dependencies: [("dep".parse().unwrap(), "1.0.0".parse().unwrap())]
                    .into_iter()
                    .collect(),
            },
            PackageVersionMetadata {
                name: "dep".parse().unwrap(),
                version: "1.0.0".parse().unwrap(),
                dependencies: BTreeMap::new(),
            },
        ],
    )
    .unwrap();
    let resolved =
        resolve_graph(&[dependency], &[transitive], ResolutionOptions::default()).unwrap();
    assert_eq!(
        resolved.selected.len(),
        2,
        "transitive dependency must be selected"
    );

    let dep = LockedPackage::new("https://registry.npmjs.org", "dep", "1.0.0", "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", tree_digest.as_str()).unwrap();
    let root = LockedPackage::new("https://registry.npmjs.org", "demo", "1.0.0", "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", tree_digest.as_str()).unwrap();
    let mut lock_a = Lockfile::new(tree_digest.as_str()).unwrap();
    lock_a.insert_package(dep.clone()).unwrap();
    let root_key = root.key();
    lock_a.insert_package(root).unwrap();
    lock_a.set_roots([root_key]).unwrap();
    let encoded_a = lock_a.to_json().unwrap();
    let mut lock_b = Lockfile::new(tree_digest.as_str()).unwrap();
    lock_b.insert_package(dep).unwrap();
    let root_b = LockedPackage::new("https://registry.npmjs.org", "demo", "1.0.0", "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", tree_digest.as_str()).unwrap();
    let root_b_key = root_b.key();
    lock_b.insert_package(root_b).unwrap();
    lock_b.set_roots([root_b_key]).unwrap();
    assert_eq!(encoded_a, lock_b.to_json().unwrap());
    assert!(encoded_a.contains("\"dep\""));

    let client = RegistryClient::new(SnapshotTransport);
    assert!(matches!(
        client.snapshot(&npm_origin, FetchMode::Offline),
        Err(tapid_registry_client::ClientError::NetworkDisabled(
            FetchMode::Offline
        ))
    ));
    assert!(matches!(
        client.snapshot(&npm_origin, FetchMode::Frozen),
        Err(tapid_registry_client::ClientError::NetworkDisabled(
            FetchMode::Frozen
        ))
    ));
    assert_eq!(
        encoded_a,
        lock_b.to_json().unwrap(),
        "frozen replay is byte-stable"
    );
    assert!(fixture.url("/demo").starts_with("http://127.0.0.1:"));
}
