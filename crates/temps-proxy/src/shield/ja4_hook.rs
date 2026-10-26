// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! JA4 at the control plane's TLS listener (ADR 066, I6).
//!
//! OpenSSL drops ClientHello extensions it does not know, so its view gives
//! no standard JA4. Instead, Pingora's pre-TLS hook reads the ClientHello
//! off the socket first, computes the JA4 and rewinds every byte it read, so
//! OpenSSL then reads the connection unchanged. The hook has no way to
//! attach data to the connection, so the JA4 waits in a short-lived map
//! keyed by the ClientHello's client random until the handshake callback,
//! which can read the random back from OpenSSL, takes it.
//!
//! A client chooses its own random and cannot know another's before it is
//! sent, so a client can only collide with itself.
//!
//! Nothing here can fail a connection: a timeout, a read error or bytes
//! that are not a ClientHello are rewound and left to OpenSSL, which
//! answers them as it always has.

use std::time::Duration;

use async_trait::async_trait;
use pingora_core::listeners::PreTlsProcess;
use pingora_core::protocols::l4::stream::Stream as L4Stream;
use temps_shield::ja4::{self, Ja4, Need, Transport};
use tokio::io::{AsyncRead, AsyncReadExt};
use tracing::debug;

/// The whole ClientHello read. A TLS handshake would wait for the same
/// bytes, so this adds no new way to hold a connection open.
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// Long enough for any handshake to reach its completion callback.
const ENTRY_TTL: Duration = Duration::from_secs(30);
const MAX_ENTRIES: u64 = 100_000;

pub struct Ja4Hook {
    by_random: moka::future::Cache<[u8; 32], Ja4>,
}

impl Default for Ja4Hook {
    fn default() -> Self {
        Self {
            by_random: moka::future::Cache::builder()
                .max_capacity(MAX_ENTRIES)
                .time_to_live(ENTRY_TTL)
                .build(),
        }
    }
}

impl Ja4Hook {
    /// The JA4 of the connection whose ClientHello carried `random`, once.
    pub async fn take(&self, random: &[u8; 32]) -> Option<Ja4> {
        self.by_random.remove(random).await
    }

    async fn record(&self, hello: &ja4::ClientHello) {
        let fingerprint = ja4::ja4(hello, Transport::Tcp);
        debug!(ja4 = %fingerprint, ja4_r = %ja4::ja4_raw(hello, Transport::Tcp), "ClientHello");
        self.by_random.insert(hello.random, fingerprint).await;
    }
}

#[async_trait]
impl PreTlsProcess for Ja4Hook {
    async fn process(&self, stream: &mut L4Stream) -> pingora_core::Result<()> {
        let mut read = Vec::with_capacity(2048);
        let hello = tokio::time::timeout(READ_TIMEOUT, read_client_hello(stream, &mut read))
            .await
            .ok()
            .flatten();
        stream.rewind(&read);
        if let Some(hello) = hello {
            self.record(&hello).await;
        }
        Ok(())
    }
}

/// Read until `buf` holds the whole ClientHello, never past the record that
/// ends it. Everything read is in `buf`, whatever the outcome, and a read
/// cancelled by the timeout loses nothing: bytes join `buf` only once a
/// read has returned them.
async fn read_client_hello<S: AsyncRead + Unpin>(
    stream: &mut S,
    buf: &mut Vec<u8>,
) -> Option<ja4::ClientHello> {
    let mut chunk = [0u8; 4096];
    loop {
        match ja4::needed(buf) {
            Ok(Need::Done(_)) => return ja4::parse(buf).ok(),
            Ok(Need::More(n)) => {
                let want = n.min(chunk.len());
                let got = stream.read(&mut chunk[..want]).await.ok()?;
                if got == 0 {
                    return None;
                }
                buf.extend_from_slice(&chunk[..got]);
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// A ClientHello from curl, from the parser's Wireshark vectors, and the
    /// JA4 Wireshark gave it.
    fn curl_hello() -> (Vec<u8>, &'static str) {
        let line = include_str!("../../../temps-shield/tests/data/ja4_wireshark.txt")
            .lines()
            .find(|line| line.starts_with("curl 8 (OpenSSL), HTTP/2|"))
            .unwrap();
        let fields: Vec<&str> = line.split('|').collect();
        let hex = fields[3];
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        (bytes, "t13d3012h2_1d37bd780c83_882d495ac381")
    }

    /// Over a real socket through Pingora's stream: the JA4 is recorded under
    /// the client random, and the TLS library reads exactly what the client
    /// sent, the bytes after the hello included.
    #[tokio::test]
    async fn the_hello_is_fingerprinted_and_handed_back_whole() {
        let (hello, expected) = curl_hello();
        let mut sent = hello.clone();
        sent.extend_from_slice(b"\x14\x03\x03\x00\x01\x01 and then some");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let to_send = sent.clone();
        let client = tokio::spawn(async move {
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            // Split the hello mid-record, as a slow network would.
            socket.write_all(&to_send[..100]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            socket.write_all(&to_send[100..]).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let (socket, _) = listener.accept().await.unwrap();
        let mut stream = L4Stream::from(socket);
        let hook = Ja4Hook::default();
        hook.process(&mut stream).await.unwrap();
        client.await.unwrap();

        let mut seen = Vec::new();
        stream.read_to_end(&mut seen).await.unwrap();
        assert_eq!(seen, sent);

        let random: [u8; 32] = hello[11..43].try_into().unwrap();
        assert_eq!(
            hook.take(&random).await.map(|j| j.to_string()).as_deref(),
            Some(expected)
        );
        assert_eq!(hook.take(&random).await, None, "taken once");
    }

    /// Plain HTTP on the TLS port, and a client that stalls: nothing is
    /// recorded and every byte goes back to the TLS library.
    #[tokio::test]
    async fn anything_else_is_handed_back_untouched() {
        let mut buf = Vec::new();
        let mut http = &b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"[..];
        assert!(read_client_hello(&mut http, &mut buf).await.is_none());
        assert_eq!(buf, b"GET /");

        let (hello, _) = curl_hello();
        let (mut client, mut server) = tokio::io::duplex(64 * 1024);
        client.write_all(&hello[..300]).await.unwrap();
        let mut buf = Vec::new();
        let outcome = tokio::time::timeout(
            Duration::from_millis(50),
            read_client_hello(&mut server, &mut buf),
        )
        .await;
        assert!(outcome.is_err(), "still waiting for the rest");
        assert_eq!(buf, hello[..300]);
    }
}
