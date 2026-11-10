// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// Drain the host response before ending the bridge. A guest write-half close
/// still permits a response, but a host read-half close ends the connection:
/// waiting for an idle guest writer would retain admission permits indefinitely.
/// Each copy uses Tokio's fixed-size buffer; no request-sized allocation occurs.
pub(super) async fn forward<G, H>(guest: G, host: H) -> std::io::Result<()>
where
    G: AsyncRead + AsyncWrite + Unpin,
    H: AsyncRead + AsyncWrite + Unpin,
{
    let (mut guest_read, mut guest_write) = tokio::io::split(guest);
    let (mut host_read, mut host_write) = tokio::io::split(host);
    let upload = async {
        tokio::io::copy(&mut guest_read, &mut host_write).await?;
        host_write.shutdown().await
    };
    let response = async {
        tokio::io::copy(&mut host_read, &mut guest_write).await?;
        guest_write.shutdown().await
    };
    tokio::pin!(upload, response);
    tokio::select! {
        result = &mut response => result,
        result = &mut upload => {
            result?;
            response.await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn guest_half_close_still_receives_the_complete_host_response() {
        let (mut guest, guest_bridge) = tokio::io::duplex(64);
        let (host_bridge, mut host) = tokio::io::duplex(64);
        let task = tokio::spawn(forward(guest_bridge, host_bridge));
        let expected = vec![b'x'; 32 * 1024];
        let remote = async {
            let mut request = Vec::new();
            host.read_to_end(&mut request).await.unwrap();
            assert_eq!(request, b"request");
            host.write_all(&expected).await.unwrap();
            host.shutdown().await.unwrap();
        };
        let client = async {
            guest.write_all(b"request").await.unwrap();
            guest.shutdown().await.unwrap();
            let mut response = Vec::new();
            guest.read_to_end(&mut response).await.unwrap();
            assert_eq!(response, expected);
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(remote, client);
            task.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn host_close_drains_response_without_waiting_for_guest_eof() {
        let (mut guest, guest_bridge) = tokio::io::duplex(64);
        let (host_bridge, mut host) = tokio::io::duplex(64);
        let task = tokio::spawn(forward(guest_bridge, host_bridge));
        let expected = vec![b'x'; 32 * 1024];
        tokio::time::timeout(Duration::from_secs(2), async {
            let send = async {
                host.write_all(&expected).await.unwrap();
                host.shutdown().await.unwrap();
            };
            let receive = async {
                let mut response = Vec::new();
                guest.read_to_end(&mut response).await.unwrap();
                assert_eq!(response, expected);
            };
            tokio::join!(send, receive);
            // Neither guest nor host has dropped its read/write stream.
            task.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }
}
