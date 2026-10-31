// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A stream that serves bytes already read off it before the rest
//! (ADR 066, I6): the public HTTPS listener reads the ClientHello for its
//! JA4, then hands rustls the connection as the client sent it.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub(crate) struct Rewind<S> {
    read: Vec<u8>,
    served: usize,
    inner: S,
}

impl<S> Rewind<S> {
    pub(crate) fn new(read: Vec<u8>, inner: S) -> Self {
        Self {
            read,
            served: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Rewind<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.served < this.read.len() {
            let rest = &this.read[this.served..];
            let n = rest.len().min(buf.remaining());
            buf.put_slice(&rest[..n]);
            this.served += n;
            if this.served == this.read.len() {
                this.read = Vec::new();
                this.served = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Rewind<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// The saved bytes come first, in reads as small as the caller asks,
    /// then the stream itself; writes go straight through.
    #[tokio::test]
    async fn saved_bytes_come_first() {
        let (mut client, server) = tokio::io::duplex(64);
        client.write_all(b" world").await.unwrap();
        client.shutdown().await.unwrap();
        let mut stream = Rewind::new(b"hello".to_vec(), server);

        let mut two = [0u8; 2];
        stream.read_exact(&mut two).await.unwrap();
        assert_eq!(&two, b"he");
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).await.unwrap();
        assert_eq!(rest, b"llo world");

        stream.write_all(b"reply").await.unwrap();
        let mut reply = [0u8; 5];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"reply");
    }
}
