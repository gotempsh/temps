// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Loopback ports the process picked for itself, held until they are served.
//!
//! A port found by binding port 0 and closing the socket is free only at that
//! moment. Anything in the network namespace can take it before the real bind,
//! such as an outbound connection or a published container port, and the
//! console then fails to start with `Address in use`.
//!
//! So the socket stays bound from the moment the port is chosen. It does not
//! listen yet: on Linux a connection is refused until the console is ready to
//! serve, rather than waiting in a backlog nothing is reading. macOS drops the
//! connection attempt instead, so there the client retries until the console
//! serves or the attempt times out.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{LazyLock, Mutex, PoisonError};

use tokio::net::TcpSocket;

/// Touched only while the server starts, never on a request path.
static RESERVATIONS: LazyLock<Mutex<HashMap<String, TcpSocket>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Pick a free loopback port and hold it. Returns the `127.0.0.1:<port>`
/// address to pass to [`take_reserved_socket`]. A reservation nobody takes is
/// released when the process exits.
pub fn reserve_loopback_address() -> std::io::Result<String> {
    let (address, socket) = bind_loopback_socket().map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("Failed to reserve a random loopback port on 127.0.0.1: {e}"),
        )
    })?;
    RESERVATIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(address.clone(), socket);
    Ok(address)
}

/// Hand over the socket holding `address`, if this process reserved it. Each
/// reservation can be taken once. An address the operator named has none, and
/// its owner binds it normally.
pub fn take_reserved_socket(address: &str) -> Option<TcpSocket> {
    RESERVATIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(address)
}

fn bind_loopback_socket() -> std::io::Result<(String, TcpSocket)> {
    // SO_REUSEADDR stays off. On Linux, two sockets that both set it can share a
    // port while neither is listening, so setting it here would let another
    // socket bind the reserved port.
    let socket = TcpSocket::new_v4()?;
    socket.bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let address = socket.local_addr()?.to_string();
    Ok((address, socket))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::net::TcpListener;

    #[test]
    fn a_reserved_port_cannot_be_bound_by_another_socket() {
        let address = reserve_loopback_address().unwrap();

        let error = TcpListener::bind(&address).expect_err("the port must stay held");

        assert_eq!(error.kind(), ErrorKind::AddrInUse, "binding {address}");
        take_reserved_socket(&address).expect("the reservation is still held");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_reserved_port_refuses_connections_until_it_is_served() {
        let address = reserve_loopback_address().unwrap();

        let error = std::net::TcpStream::connect(&address).expect_err("nothing is serving yet");

        assert_eq!(error.kind(), ErrorKind::ConnectionRefused, "{address}");
        take_reserved_socket(&address).expect("the reservation is still held");
    }

    #[tokio::test]
    async fn a_taken_reservation_serves_on_the_reserved_address() {
        let address = reserve_loopback_address().unwrap();

        let listener = take_reserved_socket(&address).unwrap().listen(16).unwrap();

        assert_eq!(listener.local_addr().unwrap().to_string(), address);
        tokio::net::TcpStream::connect(&address).await.unwrap();
        listener.accept().await.unwrap();
    }

    #[test]
    fn a_reservation_can_be_taken_once() {
        let address = reserve_loopback_address().unwrap();

        assert!(take_reserved_socket(&address).is_some());
        assert!(take_reserved_socket(&address).is_none());
    }

    #[test]
    fn an_address_that_was_not_reserved_has_no_socket() {
        assert!(take_reserved_socket("127.0.0.1:1").is_none());
    }

    #[test]
    fn each_reservation_gets_its_own_port() {
        let first = reserve_loopback_address().unwrap();
        let second = reserve_loopback_address().unwrap();

        assert_ne!(first, second);
        take_reserved_socket(&first).unwrap();
        take_reserved_socket(&second).unwrap();
    }
}
