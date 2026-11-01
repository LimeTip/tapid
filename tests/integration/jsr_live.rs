//! Explicit network acceptance lane; never included in ordinary integration runs.
use std::{
    fs,
    io::Write,
    net::TcpListener,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

// A controlled CONNECT proxy rejects every request and counts transport attempts.
// A cold install must contact it; warm replay must not contact it at all.
struct DenyProxy {
    url: String,
    calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl DenyProxy {
    fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (requests, done) = (calls.clone(), stop.clone());
        let worker = thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        requests.fetch_add(1, Ordering::SeqCst);
                        stream
                            .set_write_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("proxy accept: {error}"),
                }
            }
        });
        Self {
            url,
            calls,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for DenyProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tapid_registry_client::{
    HttpTransport, HttpsTransport, JsrRegistry, validate_jsr_artifact_manifest,
};
use tapid_test_support::{TempHome, TempProject};

fn checked(command: &mut Command) -> std::process::Output {
    let output = command
        .output()
        .expect("execute required acceptance prerequisite");
    assert!(
        output.status.success(),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn isolated_command(binary: &Path, project: &Path, home: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .current_dir(project)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"));
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    command
}

#[test]
fn jsr_live_install_exports_and_exact_replay() {
    let binary = fs::canonicalize(
        std::env::var_os("TAPID_TEST_BINARY")
            .expect("set TAPID_TEST_BINARY from source-build Cargo metadata"),
    )
    .unwrap();
    let node = fs::canonicalize(
        std::env::var_os("TAPID_TEST_NODE")
            .expect("set TAPID_TEST_NODE to an absolute Node executable"),
    )
    .unwrap();
    let project = TempProject::new("jsr-live").unwrap();
    let home = TempHome::new("jsr-live").unwrap();
    let store = project.path().join("store");
    project.write("package.json", br#"{"name":"live-jsr","version":"1.0.0","type":"module","dependencies":{"jsr:@std/path":"1.1.6","jsr:@std/internal":"1.0.14"}}"#).unwrap();
    let transport = HttpsTransport::standard().unwrap();
    let registry = JsrRegistry::new(
        HttpsTransport::standard().unwrap(),
        "https://jsr.io".parse().unwrap(),
    );
    let mut receipts = Vec::new();
    for (logical, generated, version) in [
        ("@std/path", "@jsr/std__path", "1.1.6"),
        ("@std/internal", "@jsr/std__internal", "1.0.14"),
    ] {
        let metadata_url = format!("https://npm.jsr.io/{generated}");
        let metadata = transport
            .get(&metadata_url)
            .expect("official compatibility metadata must be reachable");
        assert_eq!(metadata.status, 200);
        let metadata: Value = serde_json::from_slice(&metadata.body).unwrap();
        let artifact = registry
            .fetch(logical)
            .unwrap()
            .into_iter()
            .find(|artifact| artifact.identity.version.to_string() == version)
            .expect("pinned live version exists");
        let bytes = HttpsTransport::standard_artifact()
            .unwrap()
            .get(&artifact.artifact_url)
            .unwrap();
        assert_eq!(bytes.status, 200);
        let archive = project
            .write(format!("{}.tgz", logical.replace('/', "-")), &bytes.body)
            .unwrap();
        // Independent Node hash calculation retains declared and computed SRI in the receipt.
        let hash = checked(isolated_command(&node, project.path(), home.path()).args(["--input-type=module", "-e", "import fs from 'node:fs'; import crypto from 'node:crypto'; console.log('sha512-'+crypto.createHash('sha512').update(fs.readFileSync(process.argv[1])).digest('base64'));", archive.to_str().unwrap()]));
        let computed = String::from_utf8(hash.stdout).unwrap().trim().to_owned();
        assert_eq!(artifact.integrity.as_ref().unwrap().as_str(), computed);
        let extracted = project
            .path()
            .join(format!("extracted-{}", logical.replace('/', "-")));
        tapid_archive::extract_to(
            &bytes.body,
            tapid_archive::ArchiveFormat::TarGz,
            &extracted,
            tapid_archive::ArchiveLimits::default(),
        )
        .unwrap();
        let manifest = fs::read(extracted.join("package/package.json")).unwrap();
        validate_jsr_artifact_manifest(logical, version, &artifact.artifact_url, &manifest)
            .unwrap();
        receipts.push(json!({"metadataUrl":metadata_url,"selectedMetadata":metadata["versions"][version],"artifactUrl":artifact.artifact_url,"declaredIntegrity":artifact.integrity.unwrap().to_string(),"computedIntegrity":computed,"archiveBytes":bytes.body.len(),"treeDigest":tapid_archive::canonical_tree_digest(&extracted).unwrap(),"generatedManifest":serde_json::from_slice::<Value>(&manifest).unwrap()}));
    }
    let installed = checked(
        isolated_command(&binary, project.path(), home.path()).args([
            "install",
            "--store-dir",
            store.to_str().unwrap(),
        ]),
    );
    let lock_bytes = fs::read(project.path().join("tapid.lock")).unwrap();
    let lock =
        tapid_lockfile::Lockfile::from_json(std::str::from_utf8(&lock_bytes).unwrap()).unwrap();
    assert_eq!(lock.packages().len(), 2);
    let lock_value: Value = serde_json::from_slice(&lock_bytes).unwrap();
    assert!(
        lock_value["packages"]
            .as_object()
            .unwrap()
            .keys()
            .all(|key| key.starts_with("https://jsr.io|@std/"))
    );
    let path = lock_value["packages"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(key, _)| key.contains("|@std/path@"))
        .unwrap()
        .1;
    assert!(
        path["dependencies"]["@jsr/std__internal"]
            .as_str()
            .unwrap()
            .starts_with("https://jsr.io|@std/internal@1.0.14|")
    );
    for receipt in &receipts {
        let generated = receipt["selectedMetadata"]["name"].as_str().unwrap();
        let logical = tapid_registry_client::jsr_dependency_name(generated)
            .unwrap()
            .unwrap();
        let package = lock_value["packages"]
            .as_object()
            .unwrap()
            .values()
            .find(|package| package["name"] == logical.as_str())
            .unwrap();
        assert_eq!(package["artifactUrl"], receipt["artifactUrl"]);
        assert_eq!(package["artifactIntegrity"], receipt["declaredIntegrity"]);
        assert_eq!(package["treeDigest"], receipt["treeDigest"]);
    }
    let probe = "import {join} from '@std/path'; import * as internal from '@std/internal'; import assert from 'node:assert/strict'; import {sep} from 'node:path'; assert.ok(Object.keys(internal).length > 0); assert.equal(join('alpha','beta'), 'alpha'+sep+'beta'); console.log('TAPID_JSR_NODE_OK');";
    let node_install = checked(
        isolated_command(&node, project.path(), home.path())
            .current_dir(project.path())
            .args(["--input-type=module", "-e", probe]),
    );
    // A rejecting proxy prevents HTTPS acquisition. The cold control
    // proves the production HTTP client honors it before warm replay uses it.
    let proxy = DenyProxy::start();
    let cold = TempProject::new("jsr-live-cold-control").unwrap();
    cold.write(
        "package.json",
        &fs::read(project.path().join("package.json")).unwrap(),
    )
    .unwrap();
    let denied = isolated_command(&binary, cold.path(), home.path())
        .args([
            "install",
            "--store-dir",
            cold.path().join("store").to_str().unwrap(),
        ])
        .env("HTTPS_PROXY", &proxy.url)
        .env("ALL_PROXY", &proxy.url)
        .env("NO_PROXY", "")
        .output()
        .unwrap();
    assert!(
        !denied.status.success(),
        "network prohibition control unexpectedly installed"
    );
    let cold_attempts = proxy.calls.swap(0, Ordering::SeqCst);
    assert!(
        cold_attempts > 0,
        "cold control must prove that HTTP uses the denying proxy"
    );
    let mut replay_receipts = Vec::new();
    for mode in ["--offline", "--frozen"] {
        fs::remove_dir_all(project.path().join("node_modules")).unwrap();
        let replay = checked(
            isolated_command(&binary, project.path(), home.path())
                .args(["install", mode, "--store-dir", store.to_str().unwrap()])
                .env("HTTPS_PROXY", &proxy.url)
                .env("ALL_PROXY", &proxy.url)
                .env("NO_PROXY", ""),
        );
        assert_eq!(
            fs::read(project.path().join("tapid.lock")).unwrap(),
            lock_bytes
        );
        let node_replay = checked(
            isolated_command(&node, project.path(), home.path())
                .current_dir(project.path())
                .args(["--input-type=module", "-e", probe]),
        );
        replay_receipts.push(json!({"mode":mode,"stdout":String::from_utf8_lossy(&replay.stdout),"stderr":String::from_utf8_lossy(&replay.stderr),"nodeStdout":String::from_utf8_lossy(&node_replay.stdout)}));
    }
    assert_eq!(
        proxy.calls.load(Ordering::SeqCst),
        0,
        "replay attempted network acquisition"
    );
    let receipt = json!({"cliSha256":format!("{:x}",Sha256::digest(fs::read(&binary).unwrap())),"nodeInstallStdout":String::from_utf8_lossy(&node_install.stdout),"replayReceipts":replay_receipts,"coldProxyStderr":String::from_utf8_lossy(&denied.stderr),"retrievedAtUnixSeconds":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),"platform":std::env::consts::OS,"nodeVersion":String::from_utf8(checked(isolated_command(&node, project.path(), home.path()).arg("--version")).stdout).unwrap().trim(),"cliInstallStdout":String::from_utf8_lossy(&installed.stdout),"cliInstallStderr":String::from_utf8_lossy(&installed.stderr),"coldProxyAttempts":cold_attempts,"replayProxyAttempts":proxy.calls.load(Ordering::SeqCst),"packages":receipts,"install":"passed","nodeExports":"passed","offlineReplay":"passed","frozenReplay":"passed","coldProxyControl":"failed as required","lock":lock_value});
    if let Some(destination) = std::env::var_os("TAPID_JSR_LIVE_RECEIPT") {
        fs::write(destination, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    }
    println!("TAPID_JSR_LIVE_OK {}", receipt);
}
