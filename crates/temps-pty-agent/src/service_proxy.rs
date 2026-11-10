// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Credential-free guest transport. The worker mounts only this sandbox's
//! attachment directory read-only. All authorization and TLS interception stay
//! on the host; this relay never accepts an identity or token from a client.
use std::{path::PathBuf, time::Duration};
use tokio::{
    net::{TcpListener, UnixStream},
    task::JoinSet,
};

pub const SOCKET_PATH: &str = "/run/temps-service-access/proxy.sock";
pub const LISTEN_ADDRESS: &str = "127.0.0.1:3128";
const MAX_CONNECTIONS: usize = 32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("Sandbox service proxy could not {operation} at {address}: {source}")]
    Listener {
        operation: &'static str,
        address: String,
        #[source]
        source: std::io::Error,
    },
}

/// A guest cannot expose the relay to other sandboxes by changing its bind
/// address. The worker remains the authority even if a guest replaces this
/// process or connects directly to its mounted Unix socket.
pub async fn run() -> Result<(), ProxyError> {
    let listener =
        TcpListener::bind(LISTEN_ADDRESS)
            .await
            .map_err(|source| ProxyError::Listener {
                operation: "listen",
                address: LISTEN_ADDRESS.into(),
                source,
            })?;
    serve(listener, PathBuf::from(SOCKET_PATH), MAX_CONNECTIONS).await
}

async fn serve(listener: TcpListener, socket: PathBuf, limit: usize) -> Result<(), ProxyError> {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let (mut guest, _) = accepted.map_err(|source| ProxyError::Listener {
                    operation: "accept a connection",
                    address: LISTEN_ADDRESS.into(),
                    source,
                })?;
                // Bound tasks before spawning. No queue or buffering of HTTP
                // bodies: Tokio's bidirectional copy uses two fixed buffers.
                while connections.try_join_next().is_some() {}
                if connections.len() >= limit {
                    drop(guest);
                    continue;
                }
                let path = socket.clone();
                connections.spawn(async move {
                    let host = match tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(path)).await {
                        Ok(Ok(host)) => host,
                        Ok(Err(_)) | Err(_) => {
                            tracing::debug!("Sandbox service proxy host attachment is unavailable");
                            return;
                        }
                    };
                    // Never log bytes or request destinations, including on
                    // errors: the stream can contain sensitive workload data.
                    if forward(&mut guest, host).await.is_err() {
                        tracing::debug!("Sandbox service proxy connection closed");
                    }
                });
            }
        }
    }
    // Dropping this future drops JoinSet and aborts every owned connection.
}

// A host close is terminal (including revocation). Do not retain an admission
// slot waiting for an idle or malicious guest to close its write half. A guest
// half-close, in contrast, must still allow the host's final response through.
async fn forward(guest: &mut tokio::net::TcpStream, mut host: UnixStream) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let (mut guest_read, mut guest_write) = guest.split();
    let (mut host_read, mut host_write) = host.split();
    let upload = async {
        tokio::io::copy(&mut guest_read, &mut host_write).await?;
        host_write.shutdown().await
    };
    let download = tokio::io::copy(&mut host_read, &mut guest_write);
    tokio::pin!(download);
    tokio::select! {
        result = upload => { result?; download.await?; },
        result = &mut download => { result?; },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpStream, UnixListener},
    };

    async fn relay(
        socket: PathBuf,
        limit: usize,
    ) -> (
        TcpStream,
        tokio::task::JoinHandle<Result<(), ProxyError>>,
        std::net::SocketAddr,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(serve(listener, socket, limit));
        (TcpStream::connect(addr).await.unwrap(), task, addr)
    }

    async fn closed(stream: &mut TcpStream) {
        let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut [0u8; 1]))
            .await
            .expect("relay must close promptly");
        assert!(
            matches!(result, Ok(0) | Err(_)),
            "unexpected data: {result:?}"
        );
    }

    #[tokio::test]
    async fn forwards_opaque_bytes_and_host_revocation_closes_guest() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("proxy.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (mut guest, task, addr) = relay(socket, 1).await;
        let (mut host, _) = listener.accept().await.unwrap();
        guest
            .write_all(b"CONNECT api.example.test:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        // Assert exact transport, with no injected guest identity headers.
        let expected = b"CONNECT api.example.test:443 HTTP/1.1\r\n\r\n";
        let mut bytes = vec![0; expected.len()];
        host.read_exact(&mut bytes).await.unwrap();
        assert_eq!(bytes, expected);
        host.write_all(b"opaque response").await.unwrap();
        let mut response = [0; 15];
        guest.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, b"opaque response");
        drop(host);
        closed(&mut guest).await;
        // The guest deliberately remains alive. The closed host must release
        // its slot so a later grant can accept new traffic.
        let _next = TcpStream::connect(addr).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn guest_half_close_preserves_the_final_host_response() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("proxy.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (mut guest, task, _) = relay(socket, 1).await;
        let (mut host, _) = listener.accept().await.unwrap();
        guest.write_all(b"request").await.unwrap();
        guest.shutdown().await.unwrap();
        let mut request = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), host.read_to_end(&mut request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request, b"request");
        host.write_all(b"final response").await.unwrap();
        drop(host);
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), guest.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response, b"final response");
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn unavailable_host_fails_closed_without_waiting_for_client_input() {
        let dir = tempfile::tempdir().unwrap();
        let (mut guest, task, _) = relay(dir.path().join("missing.sock"), 32).await;
        closed(&mut guest).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn saturation_refuses_extra_clients_and_cancellation_cuts_owned_streams() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("proxy.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (mut guest, task, addr) = relay(socket, 1).await;
        let (mut host, _) = listener.accept().await.unwrap();
        let mut extra = TcpStream::connect(addr).await.unwrap();
        closed(&mut extra).await;
        // Saturation cannot evict an admitted connection.
        guest.write_all(b"a").await.unwrap();
        let mut byte = [0];
        host.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte, [b'a']);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        closed(&mut guest).await;
        let n = tokio::time::timeout(Duration::from_secs(2), host.read(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, 0);
    }
}
