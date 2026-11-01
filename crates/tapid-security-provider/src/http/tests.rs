use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    thread,
};

fn request() -> SecurityRequest {
    serde_json::from_value(
        serde_json::from_slice::<serde_json::Value>(include_bytes!(
            "../../tests/fixtures/advisory-v2.json"
        ))
        .unwrap()["claims"]["request"]
            .clone(),
    )
    .unwrap()
}

// This server reads the full POST inventory, unlike the registry GET fixture.
// All certificates and keys are public, deterministic loopback-only test data.
fn server(reply: String, delay: Duration) -> (String, thread::JoinHandle<(String, Vec<u8>)>) {
    serve(reply, delay, Duration::ZERO)
}

fn serve(
    reply: String,
    delay: Duration,
    body_delay: Duration,
) -> (String, thread::JoinHandle<(String, Vec<u8>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("https://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until =>
                {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("loopback accept failed: {error}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(
                include_bytes!("../../tests/fixtures/local-test-cert.der").to_vec(),
            )],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                include_bytes!("../../tests/fixtures/local-test-key.der").to_vec(),
            )),
        )
        .unwrap();
        let mut stream = rustls::StreamOwned::new(
            rustls::ServerConnection::new(Arc::new(config)).unwrap(),
            socket,
        );
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 16 * 1024);
        }
        let headers = String::from_utf8(headers).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap();
        assert!(length < 64 * 1024);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        thread::sleep(delay);
        let (head, tail) = reply.split_once("\r\n\r\n").unwrap();
        let _ = write!(stream, "{head}\r\n\r\n");
        let _ = stream.flush();
        thread::sleep(body_delay);
        let _ = stream.write_all(tail.as_bytes());
        let _ = stream.flush();
        (headers, body)
    });
    (origin, handle)
}

fn provider(origin: &str) -> HttpsProvider {
    let config = ProviderConfig::parse(
        &serde_json::to_vec(&serde_json::json!({
            "version": 1, "origin": origin, "headlessCredentialEnv": "AUDIT_CI", "cache": "disabled"
        }))
        .unwrap(),
    )
    .unwrap();
    let mut provider =
        HttpsProvider::new(config, Duration::from_secs(2), Duration::from_secs(5)).unwrap();
    // Add only the frozen fixture CA; use the production HTTPS and redirect policy.
    provider.client = client_builder(Duration::from_secs(2))
        .add_root_certificate(
            reqwest::Certificate::from_der(include_bytes!(
                "../../tests/fixtures/local-test-ca.der"
            ))
            .unwrap(),
        )
        .build()
        .unwrap();
    provider
}

#[test]
fn query_posts_exact_inventory_and_selected_credential_without_identity_refresh() {
    let envelope = include_str!("../../tests/fixtures/advisory-v2.json");
    let (origin, handle) = server(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{envelope}",
            envelope.len()
        ),
        Duration::ZERO,
    );
    let provider = provider(&origin);
    let request = request();
    let mut response = provider
        .query(
            &request,
            std::slice::from_ref(&request),
            CredentialMode::Headless,
            &mut |name| {
                assert_eq!(name, "AUDIT_CI");
                Some("fixture-token".into())
            },
        )
        .unwrap();
    let mut bytes = String::new();
    response.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, envelope);
    let (headers, body) = handle.join().unwrap();
    assert!(headers.starts_with("POST /v1/security/evidence/query HTTP/1.1\r\n"));
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-token\r\n")
    );
    assert!(headers.contains("accept-encoding: identity\r\n"));
    assert!(headers.contains("cache-control: no-store\r\n"));
    assert_eq!(
        serde_json::from_slice::<SecurityRequest>(&body).unwrap(),
        request
    );
}

#[test]
fn redirects_never_contact_the_destination_or_forward_inventory() {
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    destination.set_nonblocking(true).unwrap();
    let (origin, handle) = server(
        format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: https://{}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            destination.local_addr().unwrap()
        ),
        Duration::ZERO,
    );
    let provider = provider(&origin);
    let request = request();
    assert!(matches!(
        provider.query(
            &request,
            std::slice::from_ref(&request),
            CredentialMode::Headless,
            &mut |_| Some("fixture-token".into())
        ),
        Err(RetrievalError::Unavailable)
    ));
    handle.join().unwrap();
    assert_eq!(
        destination.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn unavailable_status_and_encoded_bodies_fail_closed() {
    for reply in [
        "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ] {
        let (origin, handle) = server(reply.into(), Duration::ZERO);
        let provider = provider(&origin);
        let request = request();
        assert!(matches!(
            provider.query(
                &request,
                std::slice::from_ref(&request),
                CredentialMode::Anonymous,
                &mut |_| panic!("anonymous")
            ),
            Err(RetrievalError::Unavailable)
        ));
        handle.join().unwrap();
    }
}

#[test]
fn collection_deadline_bounds_waiting_for_response_headers() {
    let (origin, handle) = server(
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        Duration::from_millis(300),
    );
    let mut provider = provider(&origin);
    provider.deadline = Instant::now() + Duration::from_millis(100);
    let request = request();
    let start = Instant::now();
    assert!(matches!(
        provider.query(
            &request,
            std::slice::from_ref(&request),
            CredentialMode::Anonymous,
            &mut |_| panic!("anonymous")
        ),
        Err(RetrievalError::Unavailable)
    ));
    assert!(start.elapsed() < Duration::from_secs(2));
    handle.join().unwrap();
}

#[test]
fn collection_deadline_also_bounds_stalled_response_bodies() {
    let (origin, handle) = serve(
        "HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx".into(),
        Duration::ZERO,
        Duration::from_millis(300),
    );
    let mut provider = provider(&origin);
    provider.deadline = Instant::now() + Duration::from_millis(100);
    let request = request();
    let start = Instant::now();
    let mut response = provider
        .query(
            &request,
            std::slice::from_ref(&request),
            CredentialMode::Anonymous,
            &mut |_| panic!("anonymous"),
        )
        .unwrap();
    assert!(response.read_to_end(&mut Vec::new()).is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    handle.join().unwrap();
}
