//! Adversarial tests against [`EgressProxy`] — the choke point that enforces a
//! [`NetworkPolicy`] allow-list ([security §5](../../docs/07-tool-runtime/security-isolation.md)).
//!
//! Unlike `sandbox_backends.rs`, these need no `docker`/`runsc`: they drive the
//! proxy directly over a raw `TcpStream`, playing the part of a sandboxed workload
//! trying to reach a host its policy doesn't allow. They run unconditionally in CI.
//!
//! Known gap (tracked, not asserted here): a container on `bridge` networking can
//! bypass the proxy entirely by ignoring `HTTPS_PROXY` and dialing out directly —
//! this is the documented "L3 egress bypass-blocking" item in
//! docs/07-tool-runtime/security-isolation.md §5, deferred
//! past v0.3. These tests instead confirm the proxy itself — the boundary that
//! *is* implemented — can't be tricked into tunneling to a host its policy denies.

use apex_tools::{EgressProxy, NetworkPolicy};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

async fn connect_to_proxy(proxy: &EgressProxy) -> TcpStream {
    TcpStream::connect(("127.0.0.1", proxy.port()))
        .await
        .expect("connect to proxy")
}

/// Send raw bytes to the proxy and read back whatever it writes before closing (or
/// a 4 KiB cap), bounded by a timeout so a hung proxy fails the test instead of the
/// suite.
async fn send_and_read(stream: &mut TcpStream, request: &[u8]) -> String {
    stream.write_all(request).await.expect("write request");
    let mut buf = vec![0u8; 4096];
    let n = timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("proxy response timed out")
        .expect("read response");
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

#[tokio::test]
async fn denies_ip_literal_bypass_of_hostname_allowlist() {
    // A classic egress-filter bypass: allow-list a hostname, then dial the same
    // target by its IP literal, hoping the filter only pattern-matches names. The
    // proxy's `allows_host` is a plain string compare, so an IP literal must never
    // match a hostname entry.
    let upstream = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind upstream");
    let upstream_port = upstream.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = upstream.accept().await;
    });

    let policy = NetworkPolicy {
        default_deny: true,
        outbound_allow: vec!["allowed.internal".to_string()],
    };
    let proxy = EgressProxy::start(policy).await.expect("start proxy");
    let mut client = connect_to_proxy(&proxy).await;

    let request = format!("CONNECT 127.0.0.1:{upstream_port} HTTP/1.1\r\n\r\n");
    let response = send_and_read(&mut client, request.as_bytes()).await;

    assert!(
        response.starts_with("HTTP/1.1 403"),
        "IP-literal CONNECT must be denied when only a hostname is allow-listed, got: {response:?}"
    );
}

#[tokio::test]
async fn denies_non_connect_methods_used_to_sidestep_host_checking() {
    // Plain-HTTP proxy forwarding (GET/POST with an absolute-URI) is out of scope —
    // only CONNECT tunneling is supported. An attacker sending GET is trying to get
    // the proxy to fetch on its behalf instead of tunneling, potentially confusing
    // host-check logic written only for CONNECT. Must be refused outright.
    let policy = NetworkPolicy {
        default_deny: true,
        outbound_allow: vec!["allowed.internal".to_string()],
    };
    let proxy = EgressProxy::start(policy).await.expect("start proxy");
    let mut client = connect_to_proxy(&proxy).await;

    let request = b"GET http://allowed.internal/ HTTP/1.1\r\nHost: allowed.internal\r\n\r\n";
    let response = send_and_read(&mut client, request).await;

    assert!(
        response.starts_with("HTTP/1.1 405"),
        "non-CONNECT methods must be rejected, got: {response:?}"
    );
}

#[tokio::test]
async fn denies_malformed_connect_request_without_crashing_the_proxy() {
    // A CONNECT line with no target at all — malformed input an adversary might
    // send hoping to trigger a panic or an out-of-bounds host parse. Must resolve
    // to a clean deny, and the proxy must still be alive to serve the next client.
    let policy = NetworkPolicy {
        default_deny: true,
        outbound_allow: vec!["allowed.internal".to_string()],
    };
    let proxy = EgressProxy::start(policy).await.expect("start proxy");

    let mut client = connect_to_proxy(&proxy).await;
    let response = send_and_read(&mut client, b"CONNECT \r\n\r\n").await;
    assert!(
        response.starts_with("HTTP/1.1 403"),
        "an empty CONNECT target must be denied, not crash the proxy, got: {response:?}"
    );

    // The proxy task must still be accepting connections after the malformed request.
    let mut second = connect_to_proxy(&proxy).await;
    let response2 = send_and_read(&mut second, b"CONNECT also.bad:443\r\n\r\n").await;
    assert!(
        response2.starts_with("HTTP/1.1 403"),
        "proxy must still be serving after a malformed request, got: {response2:?}"
    );
}

#[tokio::test]
async fn denies_oversized_header_flood_without_hanging() {
    // A header-smuggling / memory-exhaustion attempt: keep sending bytes with no
    // terminating blank line. The proxy caps the head buffer at 8 KiB and must
    // respond 400 and stop reading rather than buffering unbounded data or hanging.
    let policy = NetworkPolicy {
        default_deny: true,
        outbound_allow: vec!["allowed.internal".to_string()],
    };
    let proxy = EgressProxy::start(policy).await.expect("start proxy");
    let mut client = connect_to_proxy(&proxy).await;

    // Just over the proxy's 8 KiB head cap, in one write small enough to land
    // entirely in the OS send buffer without blocking (so the write completes
    // before the proxy reacts and closes the connection).
    let flood = vec![b'A'; 8 * 1024 + 200];
    let outcome = timeout(Duration::from_secs(5), async {
        client.write_all(&flood).await.expect("write flood");
        let mut buf = vec![0u8; 4096];
        client.read(&mut buf).await.map(|n| buf[..n].to_vec())
    })
    .await
    .expect("proxy must actively terminate the connection instead of hanging under an oversized header flood");

    // The proxy closes the socket the instant it trips the cap, with unread bytes
    // still in its receive buffer — on some platforms the OS turns that abrupt
    // close into an RST rather than delivering the "400 Bad Request" body first.
    // Either way, the connection was terminated promptly (no hang, no unbounded
    // buffering); a body, when we do get one, must say 400.
    match outcome {
        Ok(bytes) => {
            let response = String::from_utf8_lossy(&bytes);
            assert!(
                response.starts_with("HTTP/1.1 400"),
                "an oversized, unterminated header block must be rejected, got: {response:?}"
            );
        }
        Err(e) => {
            assert_eq!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset,
                "expected either a 400 response or a connection reset, got: {e:?}"
            );
        }
    }
}
