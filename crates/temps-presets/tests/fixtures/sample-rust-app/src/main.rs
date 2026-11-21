// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A minimal HTTP server on the PORT Temps injects, with no dependencies so
//! the fixture builds offline and fast.

use std::io::{Read, Write};
use std::net::TcpListener;

fn main() -> std::io::Result<()> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let listener = TcpListener::bind(("0.0.0.0", port.parse().unwrap_or(8080)))?;
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut request = [0u8; 1024];
        let _ = stream.read(&mut request);
        let body = "sample-rust-ok";
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
    }
    Ok(())
}
