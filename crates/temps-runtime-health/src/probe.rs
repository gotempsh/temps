// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{HealthReason, Probe, ProbeDialer, RuntimeSnapshot};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    time::{timeout_at, Duration, Instant},
};

/// Network operations are bounded by the attempt/lease deadline. HTTP reads
/// bounded informational headers and a fixed-size final status line. It follows
/// no redirect and never captures response bodies. `dialer` opens the
/// connection (the inspector's route to the endpoint), inside the deadline.
pub(crate) async fn network_probe(
    probe: &Probe,
    snapshot: &RuntimeSnapshot,
    deadline: Instant,
    dialer: &dyn ProbeDialer,
) -> Result<(), HealthReason> {
    if Instant::now() >= deadline {
        return Err(HealthReason::ProbeTimeout);
    }
    let endpoint = snapshot.endpoint.ok_or(HealthReason::EndpointUnavailable)?;
    if endpoint.port() == 0 || endpoint.ip().is_unspecified() || endpoint.ip().is_multicast() {
        return Err(HealthReason::EndpointUnavailable);
    }
    let operation = async {
        let mut stream = dialer
            .connect(endpoint)
            .await
            .map_err(|_| HealthReason::ConnectionFailed)?;
        match probe {
            Probe::Tcp {} => {
                // Published-port forwarders may accept before a refused backend.
                // The connection is dropped afterwards, so reading (rather
                // than peeking) the byte loses nothing.
                let confirmation = Instant::now() + Duration::from_millis(100);
                let mut byte = [0u8; 1];
                match timeout_at(confirmation.min(deadline), stream.read(&mut byte)).await {
                    Ok(Ok(0)) | Ok(Err(_)) => Err(HealthReason::ConnectionFailed),
                    Ok(Ok(_)) => Ok(()),
                    Err(_) if confirmation <= deadline => Ok(()),
                    Err(_) => Err(HealthReason::ProbeTimeout),
                }
            }
            Probe::Http {
                path,
                expected_status,
            } => {
                let request =
                    format!("GET {path} HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\n\r\n");
                stream
                    .write_all(request.as_bytes())
                    .await
                    .map_err(|_| HealthReason::ConnectionFailed)?;
                // Preserve read-ahead when an interim and final response arrive
                // together. Headers are scanned, never accumulated in memory.
                let mut response = BufReader::with_capacity(256, stream);
                let status = read_final_status(&mut response).await?;
                if status == *expected_status {
                    Ok(())
                } else {
                    Err(HealthReason::HttpStatusMismatch)
                }
            }
            // Fail closed if a caller routes a process check to network I/O.
            Probe::Process { .. } => Err(HealthReason::EndpointUnavailable),
        }
    };
    timeout_at(deadline, operation)
        .await
        .map_err(|_| HealthReason::ProbeTimeout)?
}

const MAX_INFORMATIONAL_RESPONSES: usize = 8;
const MAX_HTTP_RESPONSE_BYTES: usize = 16 * 1024;

async fn response_byte(
    reader: &mut (impl AsyncRead + Unpin),
    remaining: &mut usize,
) -> Result<u8, HealthReason> {
    if *remaining == 0 {
        return Err(HealthReason::InvalidHttpResponse);
    }
    *remaining -= 1;
    reader.read_u8().await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            HealthReason::InvalidHttpResponse
        } else {
            HealthReason::ConnectionFailed
        }
    })
}

async fn read_final_status(reader: &mut (impl AsyncRead + Unpin)) -> Result<u16, HealthReason> {
    let mut remaining = MAX_HTTP_RESPONSE_BYTES;
    for informational in 0..=MAX_INFORMATIONAL_RESPONSES {
        let mut line = [0u8; 128];
        let mut length = 0;
        let status = loop {
            if length == line.len() {
                return Err(HealthReason::InvalidHttpResponse);
            }
            line[length] = response_byte(reader, &mut remaining).await?;
            length += 1;
            if line[length - 1] == b'\n' {
                break parse_status(&line[..length])?;
            }
        };
        if !(100..200).contains(&status) {
            return Ok(status);
        }
        // An upgrade changes protocols, so there is no final HTTP health result.
        if status == 101 {
            return Err(HealthReason::HttpStatusMismatch);
        }
        if informational == MAX_INFORMATIONAL_RESPONSES {
            return Err(HealthReason::InvalidHttpResponse);
        }
        // The status line already ended in CRLF. An empty header block needs
        // only one further CRLF; otherwise scan through its CRLFCRLF terminator.
        let mut matched = 2;
        while matched != 4 {
            let byte = response_byte(reader, &mut remaining).await?;
            matched = match (matched, byte) {
                (0 | 2, b'\r') | (1 | 3, b'\n') => matched + 1,
                (_, b'\r') => 1,
                _ => 0,
            };
        }
    }
    Err(HealthReason::InvalidHttpResponse)
}

fn parse_status(line: &[u8]) -> Result<u16, HealthReason> {
    if line.len() < 14
        || !line.ends_with(b"\r\n")
        || !(line.starts_with(b"HTTP/1.1 ") || line.starts_with(b"HTTP/1.0 "))
        || line[12] != b' '
        || !line[9..12].iter().all(u8::is_ascii_digit)
    {
        return Err(HealthReason::InvalidHttpResponse);
    }
    Ok(u16::from(line[9] - b'0') * 100
        + u16::from(line[10] - b'0') * 10
        + u16::from(line[11] - b'0'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn response_result(parts: Vec<Vec<u8>>) -> Result<(), HealthReason> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let count = socket.read(&mut request).await.unwrap();
            assert!(count > 0);
            for part in parts {
                if socket.write_all(&part).await.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        });
        let snapshot = RuntimeSnapshot {
            container_id: format!("{:064x}", 1),
            running: true,
            restart_count: 0,
            started_at: "runtime-start-1".into(),
            endpoint: Some(endpoint),
        };
        let result = network_probe(
            &Probe::Http {
                path: "/ready".into(),
                expected_status: 200,
            },
            &snapshot,
            Instant::now() + Duration::from_secs(1),
            &crate::TcpDialer,
        )
        .await;
        server.await.unwrap();
        result
    }

    #[tokio::test]
    async fn early_hints_are_followed_to_the_final_status_in_the_same_packet() {
        assert_eq!(response_result(vec![
            b"HTTP/1.1 103 Early Hints\r\nLink: </style.css>; rel=preload\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec()
        ]).await, Ok(()));
    }

    #[tokio::test]
    async fn split_interim_headers_and_multiple_informational_statuses_preserve_the_final_response()
    {
        assert_eq!(
            response_result(vec![
                b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 102 Processing\r\nX-Progress: pending\r"
                    .to_vec(),
                b"\n\r\nHTTP/1.1 103 Early Hints\r\nLink: </style.css>; rel=preload\r\n\r".to_vec(),
                b"\nHTTP/1.1 20".to_vec(),
                b"0 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
            ])
            .await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn early_hints_do_not_mask_a_final_failure_or_protocol_upgrade() {
        for (response, reason) in [
            (
                &b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 503 Unavailable\r\n\r\n"[..],
                HealthReason::HttpStatusMismatch,
            ),
            (
                &b"HTTP/1.1 101 Switching Protocols\r\n\r\nHTTP/1.1 200 OK\r\n\r\n"[..],
                HealthReason::HttpStatusMismatch,
            ),
            (
                &b"HTTP/1.1 103 Early Hints\r\n\r\n"[..],
                HealthReason::InvalidHttpResponse,
            ),
            (
                &b"HTTP/1.1 103 Early Hints\r\nLink: </style.css>\r\n"[..],
                HealthReason::InvalidHttpResponse,
            ),
        ] {
            assert_eq!(response_result(vec![response.to_vec()]).await, Err(reason));
        }
    }

    #[tokio::test]
    async fn informational_count_and_total_header_bytes_are_bounded() {
        let mut allowed = b"HTTP/1.1 103 Early Hints\r\n\r\n".repeat(MAX_INFORMATIONAL_RESPONSES);
        allowed.extend_from_slice(b"HTTP/1.1 200 OK\r\n\r\n");
        assert_eq!(response_result(vec![allowed]).await, Ok(()));

        let mut excessive =
            b"HTTP/1.1 103 Early Hints\r\n\r\n".repeat(MAX_INFORMATIONAL_RESPONSES + 1);
        excessive.extend_from_slice(b"HTTP/1.1 200 OK\r\n\r\n");
        assert_eq!(
            response_result(vec![excessive]).await,
            Err(HealthReason::InvalidHttpResponse)
        );

        let mut headers = b"HTTP/1.1 103 Early Hints\r\nLink: ".to_vec();
        headers.extend(std::iter::repeat_n(b'x', MAX_HTTP_RESPONSE_BYTES));
        headers.extend_from_slice(b"\r\n\r\nHTTP/1.1 200 OK\r\n\r\n");
        assert_eq!(
            response_result(vec![headers]).await,
            Err(HealthReason::InvalidHttpResponse)
        );
    }

    /// Answers every connection in memory with `response` (or refuses), and
    /// records the endpoint it was asked for.
    struct InMemoryDialer {
        response: Option<&'static [u8]>,
        dialed: std::sync::Mutex<Vec<std::net::SocketAddr>>,
    }

    #[async_trait::async_trait]
    impl crate::ProbeDialer for InMemoryDialer {
        async fn connect(
            &self,
            endpoint: std::net::SocketAddr,
        ) -> std::io::Result<Box<dyn crate::ProbeConnection>> {
            self.dialed.lock().unwrap().push(endpoint);
            let response = self.response.ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused")
            })?;
            let (client, mut server) = tokio::io::duplex(4096);
            tokio::spawn(async move {
                let mut request = [0; 2048];
                if response.is_empty() {
                    return; // close at once, like a forwarder whose backend refused
                }
                let _ = server.read(&mut request).await;
                let _ = server.write_all(response).await;
            });
            Ok(Box::new(client))
        }
    }

    fn snapshot_at(endpoint: std::net::SocketAddr) -> RuntimeSnapshot {
        RuntimeSnapshot {
            container_id: format!("{:064x}", 1),
            running: true,
            restart_count: 0,
            started_at: "runtime-start-1".into(),
            endpoint: Some(endpoint),
        }
    }

    /// A Mac worker publishes ports in its Docker app's VM, which the agent
    /// cannot route to: the probe must go through the inspector's dialer, to
    /// the inspected endpoint, and never dial it directly.
    #[tokio::test]
    async fn probes_connect_through_the_dialer_to_the_inspected_endpoint() {
        // TEST-NET-1: nothing answers here, so a direct dial would time out.
        let endpoint: std::net::SocketAddr = "192.0.2.10:32768".parse().unwrap();
        let dialer = InMemoryDialer {
            response: Some(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"),
            dialed: Default::default(),
        };
        let http = Probe::Http {
            path: "/".into(),
            expected_status: 200,
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        let snapshot = snapshot_at(endpoint);
        assert_eq!(
            network_probe(&http, &snapshot, deadline, &dialer).await,
            Ok(())
        );
        assert_eq!(
            network_probe(&Probe::Tcp {}, &snapshot, deadline, &dialer).await,
            Ok(())
        );
        assert_eq!(*dialer.dialed.lock().unwrap(), vec![endpoint, endpoint]);
    }

    #[tokio::test]
    async fn a_dialer_that_cannot_connect_or_is_closed_at_once_fails_the_probe() {
        let snapshot = snapshot_at("192.0.2.10:32768".parse().unwrap());
        let deadline = Instant::now() + Duration::from_secs(1);
        let refused = InMemoryDialer {
            response: None,
            dialed: Default::default(),
        };
        let closed = InMemoryDialer {
            response: Some(b""),
            dialed: Default::default(),
        };
        let http = Probe::Http {
            path: "/".into(),
            expected_status: 200,
        };
        for probe in [&Probe::Tcp {}, &http] {
            assert_eq!(
                network_probe(probe, &snapshot, deadline, &refused).await,
                Err(HealthReason::ConnectionFailed)
            );
        }
        assert_eq!(
            network_probe(&Probe::Tcp {}, &snapshot, deadline, &closed).await,
            Err(HealthReason::ConnectionFailed)
        );
        assert!(network_probe(&http, &snapshot, deadline, &closed)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn hanging_informational_headers_use_the_original_attempt_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket
                .write_all(b"HTTP/1.1 103 Early Hints\r\nLink: </style.css>")
                .await
                .unwrap();
            futures::future::pending::<()>().await;
        });
        let snapshot = RuntimeSnapshot {
            container_id: format!("{:064x}", 1),
            running: true,
            restart_count: 0,
            started_at: "runtime-start-1".into(),
            endpoint: Some(endpoint),
        };
        let result = network_probe(
            &Probe::Http {
                path: "/ready".into(),
                expected_status: 200,
            },
            &snapshot,
            Instant::now() + Duration::from_millis(100),
            &crate::TcpDialer,
        )
        .await;
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        assert_eq!(result, Err(HealthReason::ProbeTimeout));
    }
}
