#![cfg(windows)]

use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    time::Duration,
};
use tapid_runner::{
    AssuranceLevel, CleanupConfidence, EnforcementDimension, ExecutionLimits, ExecutionRequest,
    FilesystemPolicy, SandboxMode, SandboxPolicy, Termination, execute,
};
use tapid_test_support::TempProject;

#[path = "support/windows_acl.rs"]
mod windows_acl;
#[path = "support/windows_node.rs"]
mod windows_node;

#[test]
fn windows_network_denial_blocks_native_loopback_connect_in_root_and_descendant() {
    native_network_denial(false);
}

#[test]
fn windows_network_denial_blocks_native_bind_in_root_and_descendant() {
    native_network_denial(true);
}

fn native_network_denial(bind: bool) {
    for descendant in [false, true] {
        let project = TempProject::new("network-denial-project").unwrap();
        let runtime = TempProject::new("network-denial-runtime").unwrap();
        let project_root = fs::canonicalize(project.path()).unwrap();
        let runtime_root = fs::canonicalize(runtime.path()).unwrap();
        let executable = runtime_root.join("network-probe.exe");
        fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        windows_acl::initialize_inheritance(&project_root);
        windows_acl::initialize_inheritance(&runtime_root);
        let project_acl = windows_acl::read_acl(&project_root);
        let runtime_acl = windows_acl::read_acl(&runtime_root);
        let executable_acl = windows_acl::read_acl(&executable);

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let system_root = std::env::var_os("SystemRoot").expect("Windows SystemRoot is required");
        let mode = match (bind, descendant) {
            (false, false) => "root",
            (false, true) => "descendant",
            (true, false) => "bind",
            (true, true) => "descendant-bind",
        };
        let arguments = [
            "--exact",
            "windows_native_network_probe_helper",
            "--nocapture",
        ];

        // Run the same binary, endpoint, environment and descendant path outside containment.
        // SystemRoot is explicit: without it Winsock can fail provider initialization (10106),
        // which is a broken positive control rather than network-denial evidence.
        let control = Command::new(&executable)
            .args(arguments)
            .env_clear()
            .env("SystemRoot", &system_root)
            .env("TAPID_NETWORK_ENDPOINT", &endpoint)
            .env("TAPID_NETWORK_MODE", mode)
            .output()
            .unwrap();
        assert!(control.status.success(), "host control failed: {control:?}");
        assert!(
            String::from_utf8_lossy(&control.stdout).contains(if bind {
                "BIND_RESULT=bound"
            } else {
                "TCP_CONNECT_RESULT=connected"
            }),
            "the host control must connect: {control:?}"
        );
        if !bind {
            let (mut connection, _) = listener
                .accept()
                .expect("host control connection must arrive");
            connection
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut payload = String::new();
            connection.read_to_string(&mut payload).unwrap();
            assert_eq!(payload, "TAPID_NATIVE_NETWORK_CONTROL_V1");
            drop(connection);
        }

        let policy = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::ManagedTree,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            false,
            vec![
                "SystemRoot".into(),
                "TAPID_NETWORK_ENDPOINT".into(),
                "TAPID_NETWORK_MODE".into(),
            ],
            descendant,
            ExecutionLimits::new(Some(15), Some(8192), Some(4), Some(128 * 1024 * 1024)).unwrap(),
        )
        .unwrap();
        let request = ExecutionRequest::builder(executable.as_os_str())
            .args(arguments.map(OsString::from))
            .executable_search_path(&runtime_root)
            .project_root(&project_root)
            .policy(policy)
            .env("SystemRoot", system_root)
            .env("TAPID_NETWORK_ENDPOINT", endpoint)
            .env("TAPID_NETWORK_MODE", mode)
            .build()
            .unwrap();
        let outcome =
            execute(&request).expect("native network probe must execute under containment");
        let stdout = String::from_utf8_lossy(outcome.stdout());
        let stderr = String::from_utf8_lossy(outcome.stderr());
        assert_eq!(
            outcome.termination(),
            &Termination::Exited(0),
            "probe must complete rather than crash: descendant={descendant}; stdout={stdout}; stderr={stderr}"
        );
        assert!(
            stdout.contains(if bind {
                "BIND_RESULT=denied"
            } else {
                "TCP_CONNECT_RESULT=denied"
            }),
            "stdout={stdout}; stderr={stderr}"
        );
        assert!(
            !stdout.contains(if bind {
                "BIND_RESULT=bound"
            } else {
                "TCP_CONNECT_RESULT=connected"
            }),
            "stdout={stdout}; stderr={stderr}"
        );
        if bind {
            assert_eq!(stdout.matches("BIND_RESULT=denied").count(), 4, "{stdout}");
        }
        if descendant {
            assert!(
                stdout.contains("TCP_DESCENDANT_COMPLETED"),
                "descendant probe did not run: {stdout}"
            );
        }
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "contained process connected to the reachable listener"
        );
        assert!(
            outcome
                .enforcement()
                .established_evidence()
                .iter()
                .any(|evidence| { evidence.dimension() == EnforcementDimension::Network })
        );
        assert_eq!(
            outcome.completion().cleanup_confidence(),
            CleanupConfidence::KernelOwnedComplete
        );
        assert_eq!(windows_acl::read_acl(&project_root), project_acl);
        assert_eq!(windows_acl::read_acl(&runtime_root), runtime_acl);
        assert_eq!(windows_acl::read_acl(&executable), executable_acl);
    }
}

#[test]
fn windows_native_network_probe_helper() {
    let Some(endpoint) = std::env::var_os("TAPID_NETWORK_ENDPOINT") else {
        return;
    };
    match std::env::var("TAPID_NETWORK_MODE").as_deref() {
        Ok(mode @ ("descendant" | "descendant-bind")) => {
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "windows_native_network_probe_helper",
                    "--nocapture",
                ])
                .env(
                    "TAPID_NETWORK_MODE",
                    if mode == "descendant-bind" {
                        "bind"
                    } else {
                        "root"
                    },
                )
                .status()
                .expect("native network descendant must start");
            assert!(child.success(), "native network descendant failed: {child}");
            println!("TCP_DESCENDANT_COMPLETED");
        }
        Ok("bind") => {
            for endpoint in ["127.0.0.1:0", "[::1]:0"] {
                for tcp in [true, false] {
                    let result = if tcp {
                        TcpListener::bind(endpoint).map(drop)
                    } else {
                        std::net::UdpSocket::bind(endpoint).map(drop)
                    };
                    match result {
                        Ok(()) => println!("BIND_RESULT=bound endpoint={endpoint} tcp={tcp}"),
                        Err(error) => {
                            assert_eq!(
                                error.kind(),
                                std::io::ErrorKind::PermissionDenied,
                                "not bind-denial evidence: {error:?}"
                            );
                            println!("BIND_RESULT=denied endpoint={endpoint} tcp={tcp}");
                        }
                    }
                }
            }
        }
        Ok("root") => {
            let endpoint = endpoint.to_str().unwrap().parse().unwrap();
            match TcpStream::connect_timeout(&endpoint, Duration::from_secs(2)) {
                Ok(mut connection) => {
                    connection
                        .write_all(b"TAPID_NATIVE_NETWORK_CONTROL_V1")
                        .unwrap();
                    println!("TCP_CONNECT_RESULT=connected");
                }
                Err(error) => {
                    // Windows firewall denial may drop SYNs rather than return WSAEACCES.
                    // Refused/unreachable/provider-initialization failures are not containment passes.
                    assert!(
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::TimedOut
                        ),
                        "unexpected socket failure: {error:?}"
                    );
                    println!(
                        "TCP_CONNECT_RESULT=denied kind={:?} raw={:?}",
                        error.kind(),
                        error.raw_os_error()
                    );
                }
            }
        }
        other => panic!("invalid native network probe mode: {other:?}"),
    }
}

struct NodeNetworkProject {
    _project: TempProject,
    _runtime: TempProject,
    root: std::path::PathBuf,
    bin: std::path::PathBuf,
    node: std::path::PathBuf,
}
impl NodeNetworkProject {
    fn new() -> Self {
        let project = TempProject::new("network-grant-project").unwrap();
        let runtime = TempProject::new("network-grant-runtime").unwrap();
        let root = fs::canonicalize(project.path()).unwrap();
        let bin = fs::canonicalize(runtime.path()).unwrap();
        let node = bin.join("node.exe");
        fs::copy(
            std::env::var_os("TAPID_TEST_NODE").expect("native Node is required"),
            &node,
        )
        .unwrap();
        windows_acl::initialize_inheritance(&root);
        windows_acl::initialize_inheritance(&bin);
        Self {
            _project: project,
            _runtime: runtime,
            root,
            bin,
            node,
        }
    }
    fn request(
        &self,
        script: &str,
        endpoint: &str,
        network: bool,
        timeout: u64,
    ) -> ExecutionRequest {
        let policy = SandboxPolicy::new_with_assurance(
            SandboxMode::Required,
            AssuranceLevel::ManagedTree,
            FilesystemPolicy::new(vec![".".into()], vec![]).unwrap(),
            network,
            vec!["SystemRoot".into(), "TAPID_NETWORK_ENDPOINT".into()],
            true,
            ExecutionLimits::new(Some(timeout), Some(8192), Some(8), Some(256 * 1024 * 1024))
                .unwrap(),
        )
        .unwrap();
        ExecutionRequest::builder(self.node.as_os_str())
            .args(["-e", script])
            .executable_search_path(&self.bin)
            .trusted_node_runtime(&self.node)
            .project_root(&self.root)
            .policy(policy)
            .env("SystemRoot", std::env::var_os("SystemRoot").unwrap())
            .env("TAPID_NETWORK_ENDPOINT", endpoint)
            .build()
            .unwrap()
    }
}
fn assert_node_network_success(outcome: tapid_runner::ExecutionOutcome, marker: &str) {
    assert_eq!(
        outcome.termination(),
        &Termination::Exited(0),
        "stdout={}; stderr={}",
        String::from_utf8_lossy(outcome.stdout()),
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(String::from_utf8_lossy(outcome.stdout()).contains(marker));
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    assert!(
        outcome
            .enforcement()
            .established_evidence()
            .iter()
            .filter(|e| e.dimension() == EnforcementDimension::Network)
            .all(|e| e.mechanism().contains("socket broker"))
    );
}
#[test]
fn windows_network_grant_runs_stock_node_loopback_tcp() {
    for (address, descendant) in [
        ("127.0.0.1", false),
        ("127.0.0.1", true),
        ("::1", false),
        ("::1", true),
    ] {
        let fixture = NodeNetworkProject::new();
        let listener = TcpListener::bind((address, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!(
            "{{\"host\":{address:?},\"port\":{}}}",
            listener.local_addr().unwrap().port()
        );
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let host_cancel = cancel.clone();
        let host = std::thread::spawn(move || -> Result<(), String> {
            let deadline = std::time::Instant::now() + Duration::from_secs(8);
            while !host_cancel.load(std::sync::atomic::Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .map_err(|e| e.to_string())?;
                        let mut payload = [0; 4];
                        stream.read_exact(&mut payload).map_err(|e| e.to_string())?;
                        if &payload != b"NODE" {
                            return Err("wrong client payload".into());
                        }
                        stream.write_all(b"HOST").map_err(|e| e.to_string())?;
                        return Ok(());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.to_string()),
                }
                if std::time::Instant::now() > deadline {
                    return Err("Node never reached host".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err("control cancelled".into())
        });
        let payload = r#"const net=require('node:net');const opts=JSON.parse(process.env.TAPID_NETWORK_ENDPOINT);const s=net.connect(opts,()=>s.write('NODE'));let data='';s.on('data',b=>{data+=b;if(data==='HOST'){console.log('NODE_NETWORK_GRANTED');s.end()}});s.on('error',e=>{console.error(e);process.exitCode=1});"#;
        let script = if descendant {
            format!(
                "const r=require('node:child_process').spawnSync(process.execPath,['-e',{payload:?}],{{stdio:'inherit'}});if(r.error)throw r.error;process.exit(r.status??1)"
            )
        } else {
            payload.to_owned()
        };
        let outcome = execute(&fixture.request(&script, &endpoint, true, 12));
        cancel.store(true, std::sync::atomic::Ordering::Release);
        let host_result = host.join().unwrap();
        assert_node_network_success(
            outcome.expect("network=true must run contained stock Node"),
            "NODE_NETWORK_GRANTED",
        );
        host_result.unwrap();
    }
}
#[test]
fn windows_network_grant_runs_stock_node_udp_and_controlled_dns() {
    for (address, kind, dns) in [
        ("127.0.0.1", "udp4", false),
        ("::1", "udp6", false),
        ("127.0.0.1", "udp4", true),
    ] {
        let fixture = NodeNetworkProject::new();
        let socket = std::net::UdpSocket::bind((address, 0)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let endpoint = format!(
            "{{\"host\":{address:?},\"port\":{},\"kind\":{kind:?}}}",
            socket.local_addr().unwrap().port()
        );
        let host = std::thread::spawn(move || {
            let mut bytes = [0; 1500];
            let (length, peer) = socket.recv_from(&mut bytes).unwrap();
            if dns {
                assert!(length >= 17);
                let mut at = 12;
                let mut labels = Vec::new();
                while bytes[at] != 0 {
                    let n = bytes[at] as usize;
                    assert!(n <= 63 && at + 1 + n < length);
                    labels.push(String::from_utf8(bytes[at + 1..at + 1 + n].to_vec()).unwrap());
                    at += 1 + n;
                }
                at += 1;
                assert_eq!(labels.join(".").to_ascii_lowercase(), "tapid-probe.invalid");
                assert_eq!(&bytes[at..at + 4], &[0, 1, 0, 1]);
                let mut reply = bytes[..2].to_vec();
                reply.extend_from_slice(&[0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
                reply.extend_from_slice(&bytes[12..at + 4]);
                reply
                    .extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4, 127, 0, 0, 42]);
                socket.send_to(&reply, peer).unwrap();
            } else {
                assert_eq!(&bytes[..length], b"NODE");
                socket.send_to(b"HOST", peer).unwrap();
            }
        });
        let script = if dns {
            r#"const dns=require('node:dns');const p=JSON.parse(process.env.TAPID_NETWORK_ENDPOINT);const r=new dns.Resolver();r.setServers([p.host+':'+p.port]);r.resolve4('tapid-probe.invalid',(e,a)=>{if(e)throw e;if(a.join()!=='127.0.0.42')throw Error('wrong DNS reply');console.log('NODE_NETWORK_GRANTED')});"#
        } else {
            r#"const d=require('node:dgram');const p=JSON.parse(process.env.TAPID_NETWORK_ENDPOINT);const s=d.createSocket(p.kind);s.on('error',e=>{throw e});s.on('message',b=>{if(b.toString()!=='HOST')throw Error('wrong UDP reply');console.log('NODE_NETWORK_GRANTED');s.close()});s.send('NODE',p.port,p.host);"#
        };
        let outcome = execute(&fixture.request(script, &endpoint, true, 10));
        let host_result = host.join();
        assert_node_network_success(
            outcome.expect("UDP/DNS must be contained"),
            "NODE_NETWORK_GRANTED",
        );
        host_result.unwrap();
    }
}
#[test]
fn windows_network_grant_native_lookup_and_timeout_cleanup() {
    let fixture = NodeNetworkProject::new();
    let script = r#"require('node:dns').lookup('localhost',{all:true},(e,a)=>{if(e)throw e;if(!a.some(x=>x.address==='127.0.0.1'||x.address==='::1'))throw Error('wrong localhost');console.log('LOOKUP_OK')});"#;
    assert_node_network_success(
        execute(&fixture.request(script, "", true, 8)).unwrap(),
        "LOOKUP_OK",
    );
    let before = windows_acl::read_acl(&fixture.root);
    let outcome=execute(&fixture.request("require('node:net').createServer(()=>{}).listen(0,'127.0.0.1',()=>console.log('LISTENING'))","",true,1)).unwrap();
    assert_eq!(
        outcome.termination(),
        &Termination::TimedOut,
        "stderr={}",
        String::from_utf8_lossy(outcome.stderr())
    );
    assert!(String::from_utf8_lossy(outcome.stdout()).contains("LISTENING"));
    assert_eq!(
        outcome.completion().cleanup_confidence(),
        CleanupConfidence::KernelOwnedComplete
    );
    assert_eq!(windows_acl::read_acl(&fixture.root), before);
}

#[test]
fn windows_network_rejected_socket_preserves_later_requests_and_descendant_setup() {
    let fixture = NodeNetworkProject::new();
    windows_node::compile_launcher(&fixture.root.join("launcher.node"));
    let script = r#"
const assert = require('node:assert/strict');
assert.equal(require('./launcher.node').socketRecovery(), 0);
const child = require('node:child_process').spawnSync(process.execPath, ['-e', "console.log('SOCKET_RECOVERY_CHILD_OK')"], {stdio:'inherit'});
if (child.error) throw child.error;
assert.equal(child.status, 0);
"#;
    assert_node_network_success(
        execute(&fixture.request(script, "", true, 10)).unwrap(),
        "SOCKET_RECOVERY_CHILD_OK",
    );
}

#[test]
fn windows_network_native_lookup_preserves_results_freeing_and_descendant_channels() {
    let fixture = NodeNetworkProject::new();
    windows_node::compile_launcher(&fixture.root.join("launcher.node"));
    let payload = r#"
const assert=require('node:assert/strict');
assert.equal(require('./launcher.node').lookupRecovery(),0);
const dns=require('node:dns').promises;
Promise.all(Array.from({length:12},()=>dns.lookup('localhost',{all:true}))).then(results=>{
for(const addresses of results) assert(addresses.some(a=>a.address==='127.0.0.1'||a.address==='::1'));
console.log('NATIVE_LOOKUP_OK');
}).catch(e=>{console.error(e);process.exitCode=1;});
"#;
    for descendant in [false, true] {
        let script = if descendant {
            format!(
                "const r=require('node:child_process').spawnSync(process.execPath,['-e',{payload:?}],{{stdio:'inherit'}});if(r.error)throw r.error;process.exit(r.status??1)"
            )
        } else {
            payload.to_owned()
        };
        assert_node_network_success(
            execute(&fixture.request(&script, "", true, 10)).unwrap(),
            "NATIVE_LOOKUP_OK",
        );
    }
}
