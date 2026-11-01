//! Shared isolated Winsock provider control; no bind, connect, or DNS operations.

pub fn probe(mode: &str) {
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, INVALID_SOCKET, IPPROTO_TCP, SOCK_STREAM, WSA_FLAG_NO_HANDLE_INHERIT,
        WSACleanup, WSADATA, WSAEACCES, WSAEPROVIDERFAILEDINIT, WSAGetLastError, WSASocketW,
        WSAStartup, closesocket,
    };
    if !matches!(
        mode,
        "provider-host" | "provider-contained" | "provider-capability-free"
    ) {
        return;
    }
    // SAFETY: WSADATA is an output structure; successful startup is balanced below.
    let mut data: WSADATA = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { WSAStartup(0x202, &mut data) },
        0,
        "provider startup"
    );
    println!("provider:startup:ok");
    for family in [AF_INET, AF_INET6] {
        // SAFETY: fixed TCP parameters, no protocol pointer or inherited handle.
        // No bind, connect, DNS lookup, or external network service is used.
        let socket = unsafe {
            WSASocketW(
                family as i32,
                SOCK_STREAM,
                IPPROTO_TCP,
                std::ptr::null(),
                0,
                WSA_FLAG_NO_HANDLE_INHERIT,
            )
        };
        if socket == INVALID_SOCKET {
            // SAFETY: read the error immediately after the failed socket operation.
            let error = unsafe { WSAGetLastError() };
            if mode == "provider-capability-free" && error == WSAEPROVIDERFAILEDINIT {
                println!("provider:contained:socket-provider-unavailable:{family}:{error}");
                continue;
            }
            assert_ne!(mode, "provider-host", "host positive control: {error}");
            assert_eq!(error, WSAEACCES, "unexpected provider failure");
            println!("provider:contained:socket-denied:{family}");
        } else {
            // SAFETY: close exactly the socket returned above.
            assert_eq!(unsafe { closesocket(socket) }, 0);
            let label = if mode == "provider-host" {
                "host"
            } else {
                "contained"
            };
            println!("provider:{label}:socket-created:{family}");
        }
    }
    // SAFETY: balances this helper's successful WSAStartup call.
    assert_eq!(unsafe { WSACleanup() }, 0);
}
