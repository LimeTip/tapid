#[path = "support/tcp_host.rs"]
mod tcp_host;

use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{Arc, Barrier},
    time::Duration,
};
use tcp_host::TcpHost;

#[test]
fn tcp_host_waits_for_client_preparation_and_exchanges_payload() {
    for address in ["127.0.0.1", "::1"] {
        let mut host = TcpHost::start(address);
        let endpoint = host.address();
        let prepared = Arc::new(Barrier::new(2));
        let client_prepared = prepared.clone();
        let client = std::thread::spawn(move || {
            client_prepared.wait();
            let mut stream = TcpStream::connect_timeout(&endpoint, Duration::from_secs(3)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(b"NODE").unwrap();
            let mut response = [0; 4];
            stream.read_exact(&mut response).unwrap();
            assert_eq!(&response, b"HOST");
        });
        prepared.wait();
        client.join().unwrap();
        host.finish().unwrap();
        assert!(TcpStream::connect_timeout(&endpoint, Duration::from_secs(1)).is_err());
    }
}

#[test]
fn tcp_host_cancels_without_a_connection_and_joins_on_unwind() {
    let mut host = TcpHost::start("127.0.0.1");
    let endpoint = host.address();
    assert_eq!(host.finish().unwrap_err(), "control cancelled");
    assert!(TcpStream::connect_timeout(&endpoint, Duration::from_secs(1)).is_err());

    let host = TcpHost::start("127.0.0.1");
    let endpoint = host.address();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _host = host;
            panic!("execution preparation failed");
        }))
        .is_err()
    );
    assert!(TcpStream::connect_timeout(&endpoint, Duration::from_secs(1)).is_err());
}
