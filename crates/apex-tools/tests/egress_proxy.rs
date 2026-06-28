//! Offline integration tests for the allow-listing egress proxy.
//!
//! No external network: a localhost TCP server stands in for an upstream, and the
//! client speaks the HTTP CONNECT protocol to the proxy directly. The proxy must
//! tunnel to an allow-listed host and refuse a non-listed one.

use apex_tools::{EgressProxy, NetworkPolicy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Start a localhost echo server; returns its port.
async fn echo_server() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = s.read(&mut buf).await {
                    if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

/// Send a CONNECT request to the proxy and return the status line.
async fn connect_via(proxy_port: u16, target: &str) -> (String, TcpStream) {
    let mut stream = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
    stream
        .write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = vec![0u8; 256];
    let n = stream.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]).to_string();
    let status = resp.lines().next().unwrap_or("").to_string();
    (status, stream)
}

#[tokio::test]
async fn proxy_tunnels_allowlisted_host_and_refuses_others() {
    let upstream = echo_server().await;
    // Allow only loopback; the upstream runs on 127.0.0.1.
    let policy = NetworkPolicy {
        default_deny: true,
        outbound_allow: vec!["127.0.0.1".to_string()],
    };
    let proxy = EgressProxy::start(policy).await.unwrap();

    // Allow-listed host → tunnel established, and the byte stream reaches the upstream.
    let (status, mut tunnel) = connect_via(proxy.port(), &format!("127.0.0.1:{upstream}")).await;
    assert!(
        status.contains("200"),
        "expected 200 Connection Established, got: {status}"
    );
    tunnel.write_all(b"ping").await.unwrap();
    let mut echoed = [0u8; 4];
    tunnel.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ping", "tunnel should reach the echo upstream");

    // Non-allow-listed host → refused before any connection is made.
    let (status, _) = connect_via(proxy.port(), "10.255.255.1:443").await;
    assert!(
        status.contains("403"),
        "expected 403 Forbidden for a non-allow-listed host, got: {status}"
    );
}

#[tokio::test]
async fn proxy_rejects_non_connect_methods() {
    let proxy = EgressProxy::start(NetworkPolicy::default()).await.unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", proxy.port()))
        .await
        .unwrap();
    stream
        .write_all(b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n")
        .await
        .unwrap();
    let mut buf = vec![0u8; 128];
    let n = stream.read(&mut buf).await.unwrap();
    let status = String::from_utf8_lossy(&buf[..n]);
    assert!(status.contains("405"), "expected 405, got: {status}");
}
