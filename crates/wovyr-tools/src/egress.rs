//! Allow-listing egress proxy for per-host network policy enforcement.
//!
//! A deny-all [`NetworkPolicy`](crate::NetworkPolicy) maps to full network isolation
//! (`--network none`), but a non-empty **allow-list** needs a forwarding choke point:
//! this is an HTTP **CONNECT** proxy that tunnels only to allow-listed hosts and
//! refuses the rest with `403` ([security §5](../../docs/07-tool-runtime/security-isolation.md)).
//! Sandboxes run the workload pointed at the proxy via `HTTPS_PROXY`, so TLS egress
//! (the security-critical path) reaches only permitted destinations.
//!
//! Scope: HTTPS via `CONNECT` (host-level allow-listing). Plain-HTTP forwarding and
//! L3 bypass-blocking (the workload ignoring the proxy) are follow-ons.

use crate::sandbox::NetworkPolicy;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A running egress proxy. Tunnels CONNECT requests to allow-listed hosts only; the
/// listener is torn down on drop.
pub struct EgressProxy {
    addr: SocketAddr,
    handle: tokio::task::JoinHandle<()>,
}

impl EgressProxy {
    /// Start a proxy on an ephemeral port (all interfaces, so sandboxed workloads can
    /// reach it via the host gateway) enforcing `policy`'s allow-list.
    pub async fn start(policy: NetworkPolicy) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", 0)).await?;
        let addr = listener.local_addr()?;
        let policy = Arc::new(policy);
        let handle = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let policy = policy.clone();
                tokio::spawn(async move {
                    let _ = handle_connect(stream, policy).await;
                });
            }
        });
        Ok(Self { addr, handle })
    }

    /// The port the proxy is listening on.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Handle one client: parse the `CONNECT host:port` request, check the policy, and
/// either tunnel to the upstream or refuse.
async fn handle_connect(mut client: TcpStream, policy: Arc<NetworkPolicy>) -> std::io::Result<()> {
    // Read the request head (up to the blank line), bounded.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if client.read(&mut byte).await? == 0 {
            return Ok(());
        }
        head.push(byte[0]);
        if head.len() > 8192 {
            client
                .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                .await?;
            return Ok(());
        }
    }

    let request_line = String::from_utf8_lossy(&head);
    let first = request_line.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if !method.eq_ignore_ascii_case("CONNECT") {
        // Only HTTPS tunneling is supported; reject other proxy methods.
        client
            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n")
            .await?;
        return Ok(());
    }

    let (host, port) = split_host_port(target);
    if !policy.allows_host(host) {
        client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await?;
        return Ok(());
    }

    let mut upstream = match TcpStream::connect((host, port)).await {
        Ok(u) => u,
        Err(_) => {
            client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await?;
            return Ok(());
        }
    };

    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    // Splice the two streams until either side closes.
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

/// Split a `host:port` authority, defaulting to `443` (HTTPS) when no port is given.
fn split_host_port(target: &str) -> (&str, u16) {
    match target.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().unwrap_or(443)),
        None => (target, 443),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_host_port_defaults_to_https() {
        assert_eq!(split_host_port("example.com:443"), ("example.com", 443));
        assert_eq!(split_host_port("example.com"), ("example.com", 443));
        assert_eq!(split_host_port("h:8443"), ("h", 8443));
    }
}
