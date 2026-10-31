// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! JA4 against an independent implementation: real ClientHellos and the
//! values Wireshark computed for them (`data/ja4_wireshark.txt`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test helpers: panicking is how a test fails"
)]
use temps_shield::ja4::{self, Need, Transport};

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn real_client_hellos_match_wireshark() {
    let vectors = include_str!("data/ja4_wireshark.txt");
    let mut checked = 0;
    for line in vectors
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let fields: Vec<&str> = line.split('|').collect();
        let [client, expected, expected_raw, payload] = fields[..] else {
            panic!("bad vector line: {line}");
        };
        let bytes = hex(payload);
        assert!(
            matches!(ja4::needed(&bytes), Ok(Need::Done(end)) if end <= bytes.len()),
            "{client}"
        );
        let hello = ja4::parse(&bytes).unwrap_or_else(|e| panic!("{client}: {e}"));
        assert_eq!(
            ja4::ja4_raw(&hello, Transport::Tcp),
            expected_raw,
            "{client}"
        );
        assert_eq!(
            ja4::ja4(&hello, Transport::Tcp).as_str(),
            expected,
            "{client}"
        );
        checked += 1;
    }
    assert_eq!(checked, 5);
}

fn curl_hello() -> Vec<u8> {
    let line = include_str!("data/ja4_wireshark.txt")
        .lines()
        .find(|line| line.starts_with("curl 8 (OpenSSL), HTTP/2|"))
        .unwrap();
    hex(line.split('|').nth(3).unwrap())
}

/// The reader stops at the end of the hello, keeps what it read, and gives
/// the same JA4 as parsing the bytes, however the network splits them.
#[tokio::test]
async fn the_reader_stops_at_the_end_of_the_hello() {
    let hello = curl_hello();
    let mut sent = hello.clone();
    sent.extend_from_slice(b"\x14\x03\x03\x00\x01\x01");
    let (mut client, mut server) = tokio::io::duplex(64);
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        for piece in sent.chunks(7) {
            client.write_all(piece).await.unwrap();
        }
        client
    });
    let mut buf = Vec::new();
    let read = ja4::read_client_hello(&mut server, &mut buf).await.unwrap();
    assert_eq!(buf, hello, "nothing past the hello is read");
    assert_eq!(
        ja4::ja4(&read, Transport::Tcp).as_str(),
        "t13d3012h2_1d37bd780c83_882d495ac381"
    );
    drop(writer.await.unwrap());
}

/// Plain HTTP, the end of the stream and a stalled client: no hello, and
/// every byte read is kept for the TLS library.
#[tokio::test]
async fn the_reader_keeps_everything_it_read() {
    let mut buf = Vec::new();
    let mut http = &b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"[..];
    assert!(ja4::read_client_hello(&mut http, &mut buf).await.is_none());
    assert_eq!(buf, b"GET /");

    let hello = curl_hello();
    let mut buf = Vec::new();
    let mut truncated = &hello[..300];
    assert!(ja4::read_client_hello(&mut truncated, &mut buf)
        .await
        .is_none());
    assert_eq!(buf, hello[..300]);

    let (mut client, mut server) = tokio::io::duplex(64 * 1024);
    tokio::io::AsyncWriteExt::write_all(&mut client, &hello[..300])
        .await
        .unwrap();
    let mut buf = Vec::new();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        ja4::read_client_hello(&mut server, &mut buf),
    )
    .await;
    assert!(outcome.is_err(), "still waiting for the rest");
    assert_eq!(buf, hello[..300]);
}
