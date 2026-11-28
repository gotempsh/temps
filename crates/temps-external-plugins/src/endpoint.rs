// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where a running plugin answers: a private Unix socket for a plugin Temps
//! started itself, or the network address of the Temps service a plugin runs
//! as.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

/// How long connecting to a plugin service may take before the attempt is
/// abandoned. The service is on this host or one mesh hop away.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A byte stream to a plugin.
pub(crate) trait PluginIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> PluginIo for T {}

/// The address Temps reaches a running plugin on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginEndpoint {
    /// The private socket of a plugin Temps started as a process or in its
    /// own container.
    Unix(PathBuf),
    /// `host:port` of the container of the Temps service the plugin runs as.
    Tcp(String),
}

impl PluginEndpoint {
    /// Open a connection to the plugin.
    pub(crate) async fn connect(&self) -> std::io::Result<Box<dyn PluginIo>> {
        match self {
            #[cfg(unix)]
            Self::Unix(path) => Ok(Box::new(tokio::net::UnixStream::connect(path).await?)),
            #[cfg(not(unix))]
            Self::Unix(_) => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "plugin sockets need a Unix host",
            )),
            Self::Tcp(authority) => {
                match tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    tokio::net::TcpStream::connect(authority.as_str()),
                )
                .await
                {
                    Ok(Ok(stream)) => {
                        // Requests and channel frames are small and
                        // latency-bound; do not wait to coalesce them.
                        let _ = stream.set_nodelay(true);
                        Ok(Box::new(stream))
                    }
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("no connection within {} seconds", CONNECT_TIMEOUT.as_secs()),
                    )),
                }
            }
        }
    }

    /// The `Host` header value for requests sent to the plugin.
    pub(crate) fn host_header(&self) -> &str {
        match self {
            Self::Unix(_) => "localhost",
            Self::Tcp(authority) => authority,
        }
    }

    /// The socket path, for a plugin Temps started itself.
    pub fn socket_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Unix(path) => Some(path),
            Self::Tcp(_) => None,
        }
    }
}

impl fmt::Display for PluginEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unix(path) => write!(f, "socket {}", path.display()),
            Self::Tcp(authority) => write!(f, "service address {authority}"),
        }
    }
}

impl From<PathBuf> for PluginEndpoint {
    fn from(path: PathBuf) -> Self {
        Self::Unix(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn a_service_endpoint_connects_over_tcp() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let endpoint = PluginEndpoint::Tcp(listener.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut received = [0u8; 4];
            stream.read_exact(&mut received).await.unwrap();
            stream.write_all(&received).await.unwrap();
        });

        let mut stream = endpoint.connect().await.expect("the service is listening");
        stream.write_all(b"ping").await.unwrap();
        let mut echoed = [0u8; 4];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"ping");
        server.await.unwrap();

        assert_eq!(endpoint.host_header(), endpoint_authority(&endpoint));
        assert!(endpoint.socket_path().is_none());
        assert!(endpoint
            .to_string()
            .starts_with("service address 127.0.0.1:"));
    }

    fn endpoint_authority(endpoint: &PluginEndpoint) -> &str {
        match endpoint {
            PluginEndpoint::Tcp(authority) => authority,
            PluginEndpoint::Unix(_) => panic!("expected a service endpoint"),
        }
    }

    #[tokio::test]
    async fn a_service_that_is_not_listening_is_an_error_not_a_hang() {
        // Bind and drop to find a port nothing listens on.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap().to_string();
        drop(listener);

        let error = match PluginEndpoint::Tcp(address).connect().await {
            Ok(_) => panic!("nothing listens on that port"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
    }

    #[test]
    fn a_socket_endpoint_keeps_its_path() {
        let endpoint = PluginEndpoint::from(PathBuf::from("/run/plugins/reports.sock"));
        assert_eq!(
            endpoint.socket_path(),
            Some(std::path::Path::new("/run/plugins/reports.sock"))
        );
        assert_eq!(endpoint.host_header(), "localhost");
        assert_eq!(endpoint.to_string(), "socket /run/plugins/reports.sock");
    }
}
