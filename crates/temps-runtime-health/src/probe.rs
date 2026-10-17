// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{HealthReason, Probe, RuntimeSnapshot};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{timeout_at, Duration, Instant},
};

/// Network operations are bounded by the attempt/lease deadline. HTTP reads
/// only a fixed-size status line, follows no redirect and never captures bodies.
pub(crate) async fn network_probe(
    probe: &Probe,
    snapshot: &RuntimeSnapshot,
    deadline: Instant,
) -> Result<(), HealthReason> {
    if Instant::now() >= deadline {
        return Err(HealthReason::ProbeTimeout);
    }
    let endpoint = snapshot.endpoint.ok_or(HealthReason::EndpointUnavailable)?;
    if endpoint.port() == 0 || endpoint.ip().is_unspecified() || endpoint.ip().is_multicast() {
        return Err(HealthReason::EndpointUnavailable);
    }
    let operation = async {
        let mut stream = TcpStream::connect(endpoint)
            .await
            .map_err(|_| HealthReason::ConnectionFailed)?;
        match probe {
            Probe::Tcp {} => {
                // Published-port forwarders may accept before a refused backend.
                let confirmation = Instant::now() + Duration::from_millis(100);
                let mut byte = [0u8; 1];
                match timeout_at(confirmation.min(deadline), stream.peek(&mut byte)).await {
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
                let mut line = [0u8; 128];
                let mut used = 0;
                loop {
                    if used == line.len() {
                        return Err(HealthReason::InvalidHttpResponse);
                    }
                    let count = stream
                        .read(&mut line[used..])
                        .await
                        .map_err(|_| HealthReason::ConnectionFailed)?;
                    if count == 0 {
                        return Err(HealthReason::InvalidHttpResponse);
                    }
                    used += count;
                    if let Some(end) = line[..used].iter().position(|b| *b == b'\n') {
                        let status = parse_status(&line[..=end])?;
                        return if status == *expected_status {
                            Ok(())
                        } else {
                            Err(HealthReason::HttpStatusMismatch)
                        };
                    }
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
