// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! JA4 TLS client fingerprints (ADR 066, D4 and I6).
//!
//! An ingress reads the first TLS records of a connection before its TLS
//! library does, asks [`needed`] how much more it must read, [`parse`]s the
//! ClientHello and computes [`ja4`], then hands every byte back unchanged.
//! The TLS library's own view of the ClientHello is not used: OpenSSL drops
//! extensions it does not know, which changes the fingerprint.
//!
//! JA4 is the FoxIO TLS client fingerprint
//! (<https://github.com/FoxIO-LLC/ja4>), whose specification is under the
//! BSD 3-Clause licence; this is an independent implementation of it. Only
//! the TLS client fingerprint is implemented: the rest of JA4+ is under a
//! licence that restricts commercial use.
//!
//! ```text
//! t13d1516h2_8daaf6152771_e5627efa2ab1
//! a: transport, version, SNI, cipher count, extension count, ALPN
//! b: sha256 of the sorted cipher suites, first 12 hex digits
//! c: sha256 of the sorted extensions (less SNI and ALPN) "_" the signature
//!    algorithms in order, first 12 hex digits
//! ```

use std::fmt;

use sha2::{Digest, Sha256};

/// No more than this is read looking for a ClientHello, record headers
/// included. Post-quantum key shares make a current browser's about 1.8 KB.
pub const MAX_CLIENT_HELLO: usize = 16 * 1024;

const RECORD_HEADER: usize = 5;
const CONTENT_HANDSHAKE: u8 = 0x16;
const HANDSHAKE_CLIENT_HELLO: u8 = 0x01;
const HANDSHAKE_HEADER: usize = 4;

const EXT_SERVER_NAME: u16 = 0x0000;
const EXT_SIGNATURE_ALGORITHMS: u16 = 0x000d;
const EXT_ALPN: u16 = 0x0010;
const EXT_SUPPORTED_VERSIONS: u16 = 0x002b;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Not a TLS handshake record: plain HTTP, an SSLv2 hello, garbage.
    #[error("not a TLS handshake")]
    NotTls,
    #[error("the first handshake message is not a ClientHello")]
    NotClientHello,
    #[error("the ClientHello is larger than {MAX_CLIENT_HELLO} bytes")]
    TooLarge,
    #[error("the ClientHello is malformed")]
    Malformed,
}

/// How much of the connection's first bytes the ClientHello takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// At least this many more bytes are needed.
    More(usize),
    /// The ClientHello ends within the first this many bytes, on a record
    /// boundary.
    Done(usize),
}

/// How many bytes the ClientHello at the start of `buf` takes, or how many
/// more must be read to know. `buf` is the start of the connection.
pub fn needed(buf: &[u8]) -> Result<Need, ParseError> {
    let mut offset = 0;
    let mut handshake = 0usize;
    let mut message_len: Option<usize> = None;
    let mut header = [0u8; HANDSHAKE_HEADER];
    loop {
        let Some(record) = buf.get(offset..offset + RECORD_HEADER) else {
            return Ok(Need::More(offset + RECORD_HEADER - buf.len()));
        };
        if record[0] != CONTENT_HANDSHAKE || record[1] != 0x03 {
            return Err(ParseError::NotTls);
        }
        let len = usize::from(u16::from_be_bytes([record[3], record[4]]));
        if len == 0 {
            return Err(ParseError::Malformed);
        }
        let body_start = offset + RECORD_HEADER;
        let body_end = body_start + len;
        // Bounds what an ingress reads, record headers included, however
        // the client splits its hello.
        if body_end > MAX_CLIENT_HELLO {
            return Err(ParseError::TooLarge);
        }
        // Learn the message length from the first four handshake bytes,
        // which may themselves be split across records.
        if message_len.is_none() {
            let available = buf.len().min(body_end).saturating_sub(body_start);
            for (i, byte) in buf[body_start..body_start + available].iter().enumerate() {
                if handshake + i < HANDSHAKE_HEADER {
                    header[handshake + i] = *byte;
                }
            }
            if handshake + available >= HANDSHAKE_HEADER {
                if header[0] != HANDSHAKE_CLIENT_HELLO {
                    return Err(ParseError::NotClientHello);
                }
                let len = usize::from(header[1]) << 16
                    | usize::from(header[2]) << 8
                    | usize::from(header[3]);
                if HANDSHAKE_HEADER + len > MAX_CLIENT_HELLO {
                    return Err(ParseError::TooLarge);
                }
                message_len = Some(HANDSHAKE_HEADER + len);
            }
        }
        if buf.len() < body_end {
            return Ok(Need::More(body_end - buf.len()));
        }
        handshake += len;
        if let Some(total) = message_len {
            if handshake >= total {
                return Ok(Need::Done(body_end));
            }
        }
        offset = body_end;
    }
}

/// What JA4 reads from a ClientHello. GREASE values are already removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHello {
    pub legacy_version: u16,
    /// Chosen by the client, so it keys the connection until the TLS
    /// library has a session for it.
    pub random: [u8; 32],
    pub ciphers: Vec<u16>,
    /// In the order sent.
    pub extensions: Vec<u16>,
    pub sni: bool,
    /// The first ALPN protocol, if the extension carries one.
    pub alpn_first: Option<Vec<u8>>,
    pub supported_versions: Vec<u16>,
    /// In the order sent.
    pub signature_algorithms: Vec<u16>,
}

/// Parse the ClientHello at the start of `buf`, which may span several
/// records. Anything else is an error, never a panic.
pub fn parse(buf: &[u8]) -> Result<ClientHello, ParseError> {
    let Need::Done(end) = needed(buf)? else {
        return Err(ParseError::Malformed);
    };
    let message = handshake_message(&buf[..end])?;
    let mut r = Reader(&message);
    let legacy_version = r.u16()?;
    let random: [u8; 32] = r.take(32)?.try_into().map_err(|_| ParseError::Malformed)?;
    let session_id = usize::from(r.u8()?);
    r.take(session_id)?;
    let mut cipher_bytes = Reader(r.vec16()?);
    let mut ciphers = Vec::with_capacity(cipher_bytes.0.len() / 2);
    while !cipher_bytes.0.is_empty() {
        let cipher = cipher_bytes.u16()?;
        if !is_grease(cipher) {
            ciphers.push(cipher);
        }
    }
    let compression = usize::from(r.u8()?);
    r.take(compression)?;

    let mut hello = ClientHello {
        legacy_version,
        random,
        ciphers,
        extensions: Vec::new(),
        sni: false,
        alpn_first: None,
        supported_versions: Vec::new(),
        signature_algorithms: Vec::new(),
    };
    // Extensions are optional before TLS 1.3.
    if r.0.is_empty() {
        return Ok(hello);
    }
    let mut extensions = Reader(r.vec16()?);
    while !extensions.0.is_empty() {
        let kind = extensions.u16()?;
        let data = extensions.vec16()?;
        if is_grease(kind) {
            continue;
        }
        hello.extensions.push(kind);
        match kind {
            EXT_SERVER_NAME => hello.sni = true,
            EXT_ALPN => {
                let mut list = Reader(Reader(data).vec16()?);
                if !list.0.is_empty() {
                    let len = usize::from(list.u8()?);
                    hello.alpn_first = Some(list.take(len)?.to_vec());
                }
            }
            EXT_SUPPORTED_VERSIONS => {
                let mut r = Reader(data);
                let len = usize::from(r.u8()?);
                let mut versions = Reader(r.take(len)?);
                while !versions.0.is_empty() {
                    let version = versions.u16()?;
                    if !is_grease(version) {
                        hello.supported_versions.push(version);
                    }
                }
            }
            EXT_SIGNATURE_ALGORITHMS => {
                let mut algorithms = Reader(Reader(data).vec16()?);
                while !algorithms.0.is_empty() {
                    let algorithm = algorithms.u16()?;
                    if !is_grease(algorithm) {
                        hello.signature_algorithms.push(algorithm);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(hello)
}

/// The handshake message, joined from its records. Borrowed when one record
/// holds it, which is the common case.
fn handshake_message(records: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>, ParseError> {
    let mut bodies = Vec::new();
    let mut offset = 0;
    while offset < records.len() {
        let header = records
            .get(offset..offset + RECORD_HEADER)
            .ok_or(ParseError::Malformed)?;
        let len = usize::from(u16::from_be_bytes([header[3], header[4]]));
        let body = records
            .get(offset + RECORD_HEADER..offset + RECORD_HEADER + len)
            .ok_or(ParseError::Malformed)?;
        bodies.push(body);
        offset += RECORD_HEADER + len;
    }
    let joined: std::borrow::Cow<'_, [u8]> = match bodies.as_slice() {
        [one] => std::borrow::Cow::Borrowed(*one),
        many => std::borrow::Cow::Owned(many.concat()),
    };
    let len = joined
        .get(1..HANDSHAKE_HEADER)
        .map(|b| usize::from(b[0]) << 16 | usize::from(b[1]) << 8 | usize::from(b[2]))
        .ok_or(ParseError::Malformed)?;
    let end = HANDSHAKE_HEADER + len;
    if joined.len() < end {
        return Err(ParseError::Malformed);
    }
    Ok(match joined {
        std::borrow::Cow::Borrowed(b) => std::borrow::Cow::Borrowed(&b[HANDSHAKE_HEADER..end]),
        std::borrow::Cow::Owned(b) => std::borrow::Cow::Owned(b[HANDSHAKE_HEADER..end].to_vec()),
    })
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        if self.0.len() < n {
            return Err(ParseError::Malformed);
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, ParseError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ParseError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    /// A vector with a two-byte length.
    fn vec16(&mut self) -> Result<&'a [u8], ParseError> {
        let len = usize::from(self.u16()?);
        self.take(len)
    }
}

/// RFC 8701: `0x?a?a` with both bytes equal.
pub fn is_grease(value: u16) -> bool {
    value & 0x0f0f == 0x0a0a && value >> 8 == value & 0xff
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Tcp,
    Quic,
}

/// A JA4 fingerprint, always 36 ASCII characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ja4([u8; 36]);

impl Ja4 {
    pub fn as_str(&self) -> &str {
        // Only ever built from ASCII.
        std::str::from_utf8(&self.0).unwrap_or("")
    }

    /// A JA4 forwarded by a trusted proxy in a header (D4). Anything that is
    /// not exactly the format is refused.
    pub fn parse_forwarded(value: &str) -> Option<Self> {
        let value = value.trim();
        let bytes: [u8; 36] = value.as_bytes().try_into().ok()?;
        let lower_alnum = |b: u8| b.is_ascii_digit() || b.is_ascii_lowercase();
        let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        let valid = matches!(bytes[0], b't' | b'q')
            && bytes[1..3].iter().all(|&b| lower_alnum(b))
            && matches!(bytes[3], b'd' | b'i')
            && bytes[4..8].iter().all(u8::is_ascii_digit)
            && bytes[8..10].iter().all(|&b| lower_alnum(b))
            && bytes[10] == b'_'
            && bytes[11..23].iter().all(|&b| hex(b))
            && bytes[23] == b'_'
            && bytes[24..36].iter().all(|&b| hex(b));
        valid.then_some(Self(bytes))
    }
}

impl fmt::Display for Ja4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Ja4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ja4({})", self.as_str())
    }
}

/// Where a request's JA4 came from (D4). It follows the client-address
/// trust chain: a proxy that ends TLS in front of temps gives its own
/// ClientHello, so only that proxy's header can carry the visitor's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ja4Source {
    /// Computed by this ingress from the connection's ClientHello.
    Direct,
    /// `cf-ja4` from a Cloudflare address.
    Cloudflare,
    /// `CDN-JA4` from a Bunny address.
    Bunny,
    /// A trusted proxy on loopback.
    Local,
    /// A registered temps edge.
    Edge,
}

impl Ja4Source {
    /// The inverse of [`Self::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Direct,
            Self::Cloudflare,
            Self::Bunny,
            Self::Local,
            Self::Edge,
        ]
        .into_iter()
        .find(|source| source.as_str() == value)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Cloudflare => "cloudflare",
            Self::Bunny => "bunny",
            Self::Local => "local",
            Self::Edge => "edge",
        }
    }
}

/// The headers proxies in front of temps carry a visitor's JA4 in (D4):
/// Cloudflare's, Bunny's, CloudFront's and the temps edge's. Each is read
/// only from the peers that set it, and every ingress removes them all
/// before a request goes upstream, so an app never sees one a visitor
/// could have written.
pub const FORWARDED_JA4_HEADERS: [&str; 4] = [
    "cf-ja4",
    "cdn-ja4",
    "cloudfront-viewer-ja4-fingerprint",
    "x-temps-edge-client",
];

/// A request's client fingerprint. `ja4` is `None` when the source gives
/// none: plain HTTP, Cloudflare without Bot Management, most local proxies.
/// Having none is not suspicious.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientFingerprint {
    pub ja4: Option<Ja4>,
    pub source: Ja4Source,
}

pub fn ja4(hello: &ClientHello, transport: Transport) -> Ja4 {
    let mut out = [b'0'; 36];
    out[..10].copy_from_slice(&prefix(hello, transport));
    out[10] = b'_';
    out[11..23].copy_from_slice(&truncated_hash(&ciphers_string(hello)));
    out[23] = b'_';
    out[24..36].copy_from_slice(&truncated_hash(&extensions_string(hello)));
    Ja4(out)
}

/// The unhashed form, `ja4_r`, for debugging a mismatch.
pub fn ja4_raw(hello: &ClientHello, transport: Transport) -> String {
    let prefix = prefix(hello, transport);
    format!(
        "{}_{}_{}",
        String::from_utf8_lossy(&prefix),
        ciphers_string(hello),
        extensions_string(hello)
    )
}

fn prefix(hello: &ClientHello, transport: Transport) -> [u8; 10] {
    let mut out = [b'0'; 10];
    out[0] = match transport {
        Transport::Tcp => b't',
        Transport::Quic => b'q',
    };
    let version = hello
        .supported_versions
        .iter()
        .copied()
        .max()
        .unwrap_or(hello.legacy_version);
    out[1..3].copy_from_slice(match version {
        0x0304 => b"13",
        0x0303 => b"12",
        0x0302 => b"11",
        0x0301 => b"10",
        0x0300 => b"s3",
        0x0002 => b"s2",
        0xfeff => b"d1",
        0xfefd => b"d2",
        0xfefc => b"d3",
        _ => b"00",
    });
    out[3] = if hello.sni { b'd' } else { b'i' };
    out[4..6].copy_from_slice(&two_digits(hello.ciphers.len()));
    out[6..8].copy_from_slice(&two_digits(hello.extensions.len()));
    out[8..10].copy_from_slice(&alpn(hello.alpn_first.as_deref()));
    out
}

fn two_digits(n: usize) -> [u8; 2] {
    let n = n.min(99) as u8;
    [b'0' + n / 10, b'0' + n % 10]
}

/// The first and last characters of the first ALPN protocol; the first and
/// last hex digits of it when either is not alphanumeric; `00` without one.
fn alpn(first: Option<&[u8]>) -> [u8; 2] {
    let Some(value) = first.filter(|value| !value.is_empty()) else {
        return *b"00";
    };
    let (head, tail) = (value[0], value[value.len() - 1]);
    if head.is_ascii_alphanumeric() && tail.is_ascii_alphanumeric() {
        return [head, tail];
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    [HEX[usize::from(head >> 4)], HEX[usize::from(tail & 0x0f)]]
}

fn ciphers_string(hello: &ClientHello) -> String {
    let mut ciphers = hello.ciphers.clone();
    ciphers.sort_unstable();
    hex_list(&ciphers)
}

fn extensions_string(hello: &ClientHello) -> String {
    let mut extensions: Vec<u16> = hello
        .extensions
        .iter()
        .copied()
        .filter(|&kind| kind != EXT_SERVER_NAME && kind != EXT_ALPN)
        .collect();
    extensions.sort_unstable();
    let mut out = hex_list(&extensions);
    if !hello.signature_algorithms.is_empty() {
        out.push('_');
        out.push_str(&hex_list(&hello.signature_algorithms));
    }
    out
}

fn hex_list(values: &[u16]) -> String {
    let mut out = String::with_capacity(values.len() * 5);
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{value:04x}"));
    }
    out
}

/// The first 12 hex digits of the SHA-256 of `input`, or twelve zeros for
/// an empty list.
fn truncated_hash(input: &str) -> [u8; 12] {
    if input.is_empty() {
        return *b"000000000000";
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(input.as_bytes());
    let mut out = [0u8; 12];
    for (i, byte) in digest.iter().take(6).enumerate() {
        out[2 * i] = HEX[usize::from(byte >> 4)];
        out[2 * i + 1] = HEX[usize::from(byte & 0x0f)];
    }
    out
}

/// Read until `buf` holds the whole ClientHello, never past the record that
/// ends it. Everything read is in `buf`, whatever the outcome, so the
/// caller can hand every byte to the TLS library; a read cancelled by a
/// timeout loses nothing, since bytes join `buf` only once a read has
/// returned them. `None` for anything that is not a ClientHello, a read
/// error or the end of the stream.
#[cfg(feature = "tokio")]
pub async fn read_client_hello<S>(stream: &mut S, buf: &mut Vec<u8>) -> Option<ClientHello>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut chunk = [0u8; 4096];
    loop {
        match needed(buf) {
            Ok(Need::Done(_)) => return parse(buf).ok(),
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

    #[test]
    fn sources_round_trip_through_their_names() {
        for source in [
            Ja4Source::Direct,
            Ja4Source::Cloudflare,
            Ja4Source::Bunny,
            Ja4Source::Local,
            Ja4Source::Edge,
        ] {
            assert_eq!(Ja4Source::parse(source.as_str()), Some(source));
        }
        assert_eq!(Ja4Source::parse("Cloudflare"), None);
    }

    /// A ClientHello with the given parts, as one handshake message.
    struct Hello {
        version: u16,
        ciphers: Vec<u16>,
        /// (type, data), in order.
        extensions: Vec<(u16, Vec<u8>)>,
    }

    impl Hello {
        fn message(&self) -> Vec<u8> {
            let mut body = Vec::new();
            body.extend_from_slice(&self.version.to_be_bytes());
            body.extend_from_slice(&[7; 32]);
            body.push(32);
            body.extend_from_slice(&[9; 32]);
            body.extend_from_slice(&((self.ciphers.len() * 2) as u16).to_be_bytes());
            for cipher in &self.ciphers {
                body.extend_from_slice(&cipher.to_be_bytes());
            }
            body.extend_from_slice(&[1, 0]);
            let mut extensions = Vec::new();
            for (kind, data) in &self.extensions {
                extensions.extend_from_slice(&kind.to_be_bytes());
                extensions.extend_from_slice(&(data.len() as u16).to_be_bytes());
                extensions.extend_from_slice(data);
            }
            body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
            body.extend_from_slice(&extensions);
            let mut message = vec![HANDSHAKE_CLIENT_HELLO];
            message.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
            message.extend_from_slice(&body);
            message
        }

        /// The message in records of at most `chunk` bytes.
        fn records(&self, chunk: usize) -> Vec<u8> {
            let mut out = Vec::new();
            for part in self.message().chunks(chunk) {
                out.extend_from_slice(&[CONTENT_HANDSHAKE, 0x03, 0x01]);
                out.extend_from_slice(&(part.len() as u16).to_be_bytes());
                out.extend_from_slice(part);
            }
            out
        }
    }

    fn u16_list(values: &[u16], width: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let len = values.len() * 2;
        if width == 1 {
            out.push(len as u8);
        } else {
            out.extend_from_slice(&(len as u16).to_be_bytes());
        }
        for value in values {
            out.extend_from_slice(&value.to_be_bytes());
        }
        out
    }

    fn alpn_ext(protocols: &[&[u8]]) -> Vec<u8> {
        let mut list = Vec::new();
        for protocol in protocols {
            list.push(protocol.len() as u8);
            list.extend_from_slice(protocol);
        }
        let mut out = (list.len() as u16).to_be_bytes().to_vec();
        out.extend_from_slice(&list);
        out
    }

    /// The worked example in the JA4 specification
    /// (`t13d1516h2_8daaf6152771_e5627efa2ab1`), with its ciphers and
    /// extensions shuffled and GREASE added, as a browser sends them.
    fn specification_example() -> Hello {
        let mut extensions: Vec<(u16, Vec<u8>)> = vec![
            (0x0a0a, vec![]),
            (0x0000, b"\x00\x0e\x00\x00\x0bexample.com".to_vec()),
            (0x0017, vec![]),
            (0xff01, vec![0]),
            (0x000a, u16_list(&[0x1a1a, 0x001d, 0x0017], 2)),
            (0x000b, vec![1, 0]),
            (0x0023, vec![]),
            (0x0010, alpn_ext(&[b"h2", b"http/1.1"])),
            (0x0005, vec![1, 0, 0, 0, 0]),
            (
                0x000d,
                u16_list(
                    &[
                        0x0403, 0x0804, 0x0401, 0x0503, 0x0805, 0x0501, 0x0806, 0x0601,
                    ],
                    2,
                ),
            ),
            (0x0012, vec![]),
            (0x0033, vec![0, 0]),
            (0x002d, vec![1, 1]),
            (0x002b, u16_list(&[0x3a3a, 0x0304, 0x0303], 1)),
            (0x001b, vec![2, 0, 2]),
            (0x4469, vec![]),
            (0x0015, vec![0; 8]),
            (0xfafa, vec![0]),
        ];
        extensions.rotate_left(3);
        Hello {
            version: 0x0303,
            ciphers: vec![
                0x2a2a, 0x1301, 0x1302, 0x1303, 0xc02b, 0xc02f, 0xc02c, 0xc030, 0xcca9, 0xcca8,
                0xc013, 0xc014, 0x009c, 0x009d, 0x002f, 0x0035,
            ],
            extensions,
        }
    }

    #[test]
    fn the_specification_example() {
        let hello = parse(&specification_example().records(16_384)).unwrap();
        assert_eq!(
            ja4_raw(&hello, Transport::Tcp),
            "t13d1516h2_002f,0035,009c,009d,1301,1302,1303,c013,c014,c02b,c02c,c02f,c030,\
             cca8,cca9_0005,000a,000b,000d,0012,0015,0017,001b,0023,002b,002d,0033,4469,\
             ff01_0403,0804,0401,0503,0805,0501,0806,0601"
        );
        assert_eq!(
            ja4(&hello, Transport::Tcp).as_str(),
            "t13d1516h2_8daaf6152771_e5627efa2ab1"
        );
        assert_eq!(hello.random, [7; 32]);
        assert_eq!(hello.alpn_first.as_deref(), Some(&b"h2"[..]));
    }

    /// One byte per record, as a hostile or odd client may send it: the
    /// same fingerprint.
    #[test]
    fn a_hello_split_across_records() {
        let records = specification_example().records(1);
        let mut buf = Vec::new();
        let mut asked = 0;
        loop {
            match needed(&buf).unwrap() {
                Need::More(n) => {
                    assert!(n > 0);
                    let take = n.min(records.len() - buf.len());
                    buf.extend_from_slice(&records[buf.len()..buf.len() + take]);
                    asked += 1;
                }
                Need::Done(end) => {
                    assert_eq!(end, records.len());
                    break;
                }
            }
        }
        assert!(asked > 100);
        let hello = parse(&buf).unwrap();
        assert_eq!(
            ja4(&hello, Transport::Tcp).as_str(),
            "t13d1516h2_8daaf6152771_e5627efa2ab1"
        );
    }

    #[test]
    fn bytes_after_the_hello_are_left_alone() {
        let mut buf = specification_example().records(16_384);
        let end = buf.len();
        buf.extend_from_slice(&[0x14, 0x03, 0x03, 0x00, 0x01, 0x01]);
        assert_eq!(needed(&buf).unwrap(), Need::Done(end));
        assert!(parse(&buf).is_ok());
    }

    #[test]
    fn tls_1_2_without_sni_or_alpn() {
        let hello = Hello {
            version: 0x0303,
            ciphers: vec![0xc02f, 0x009c],
            extensions: vec![(0x000d, u16_list(&[0x0401], 2)), (0x000b, vec![1, 0])],
        };
        let hello = parse(&hello.records(16_384)).unwrap();
        let ja4 = ja4(&hello, Transport::Tcp);
        assert!(ja4.as_str().starts_with("t12i020200_"), "{ja4}");
        assert_eq!(
            ja4_raw(&hello, Transport::Tcp),
            "t12i020200_009c,c02f_000b,000d_0401"
        );
    }

    #[test]
    fn no_extensions_at_all() {
        let mut message = Hello {
            version: 0x0301,
            ciphers: vec![0x002f],
            extensions: vec![],
        }
        .message();
        // Drop the empty extensions block, as old clients do.
        message.truncate(message.len() - 2);
        let len = (message.len() - HANDSHAKE_HEADER) as u32;
        message[1..4].copy_from_slice(&len.to_be_bytes()[1..]);
        let mut records = vec![CONTENT_HANDSHAKE, 0x03, 0x01];
        records.extend_from_slice(&(message.len() as u16).to_be_bytes());
        records.extend_from_slice(&message);
        let hello = parse(&records).unwrap();
        assert_eq!(
            ja4(&hello, Transport::Tcp).as_str(),
            format!("t10i010000_{}_000000000000", hex12("002f"))
        );
    }

    fn hex12(input: &str) -> String {
        String::from_utf8(truncated_hash(input).to_vec()).unwrap()
    }

    #[test]
    fn alpn_values_that_are_not_alphanumeric_use_hex() {
        assert_eq!(&alpn(None), b"00");
        assert_eq!(&alpn(Some(b"")), b"00");
        assert_eq!(&alpn(Some(b"h2")), b"h2");
        assert_eq!(&alpn(Some(b"http/1.1")), b"h1");
        assert_eq!(&alpn(Some(b"h")), b"hh");
        assert_eq!(&alpn(Some(&[0xab])), b"ab");
        assert_eq!(&alpn(Some(&[0xab, 0xcd])), b"ad");
        assert_eq!(&alpn(Some(&[0x30, 0xab])), b"3b");
        assert_eq!(&alpn(Some(&[0xab, 0x34])), b"a4");
    }

    #[test]
    fn grease_is_every_0x_a_a_with_equal_bytes() {
        let grease: Vec<u16> = (0..=0xffff).filter(|&v| is_grease(v)).collect();
        assert_eq!(grease.len(), 16);
        assert!(grease.contains(&0x0a0a) && grease.contains(&0xfafa));
        assert!(!is_grease(0x0a1a));
    }

    #[test]
    fn what_is_not_a_client_hello() {
        assert_eq!(needed(b"GET / HTTP/1.1\r\n"), Err(ParseError::NotTls));
        // SSLv2-compatible hello.
        assert_eq!(
            needed(&[0x80, 0x2e, 0x01, 0x00, 0x02]),
            Err(ParseError::NotTls)
        );
        // A handshake record holding a ServerHello.
        assert_eq!(
            needed(&[0x16, 0x03, 0x03, 0x00, 0x04, 0x02, 0x00, 0x00, 0x00]),
            Err(ParseError::NotClientHello)
        );
        assert_eq!(
            needed(&[0x16, 0x03, 0x01, 0x00, 0x00]),
            Err(ParseError::Malformed)
        );
        assert_eq!(needed(&[]), Ok(Need::More(5)));
        assert_eq!(needed(&[0x16, 0x03]), Ok(Need::More(3)));
        // A ClientHello that claims more than the limit.
        assert_eq!(
            needed(&[0x16, 0x03, 0x01, 0x00, 0x04, 0x01, 0x01, 0x00, 0x00]),
            Err(ParseError::TooLarge)
        );
        // Truncated inside the message, though the record says it is whole.
        let mut short = specification_example().records(16_384);
        let body_len = u16::from_be_bytes([short[3], short[4]]) - 10;
        short[3..5].copy_from_slice(&body_len.to_be_bytes());
        short.truncate(short.len() - 10);
        assert!(matches!(needed(&short), Ok(Need::More(_))));
        assert!(parse(&short).is_err());
    }

    /// Every prefix and every single-byte change of a real hello gives a
    /// result or an error, never a panic. The fuzz target covers the rest.
    #[test]
    fn mangled_hellos_never_panic() {
        let records = specification_example().records(700);
        for end in 0..records.len() {
            let _ = needed(&records[..end]);
            let _ = parse(&records[..end]);
        }
        for i in 0..records.len() {
            for value in [0x00, 0x01, 0x7f, 0x80, 0xff] {
                let mut mangled = records.clone();
                mangled[i] = value;
                if let Ok(hello) = parse(&mangled) {
                    let _ = ja4(&hello, Transport::Tcp);
                }
            }
        }
    }

    #[test]
    fn forwarded_values_must_be_exactly_a_ja4() {
        let good = "t13d1516h2_8daaf6152771_e5627efa2ab1";
        assert_eq!(Ja4::parse_forwarded(good).unwrap().as_str(), good);
        assert!(Ja4::parse_forwarded(" t13d1516h2_8daaf6152771_e5627efa2ab1 ").is_some());
        assert!(Ja4::parse_forwarded("q13i0310h3_55b375c5d22e_cd85d2d88918").is_some());
        for bad in [
            "",
            "t13d1516h2_8daaf6152771_e5627efa2ab",
            "t13d1516h2_8daaf6152771_e5627efa2ab1x",
            "x13d1516h2_8daaf6152771_e5627efa2ab1",
            "t13x1516h2_8daaf6152771_e5627efa2ab1",
            "t13d15a6h2_8daaf6152771_e5627efa2ab1",
            "t13d1516H2_8daaf6152771_e5627efa2ab1",
            "t13d1516h2_8DAAF6152771_e5627efa2ab1",
            "t13d1516h2-8daaf6152771_e5627efa2ab1",
            "t13d1516h2_8daaf615277g_e5627efa2ab1",
            "<script>alert(1)</script>__________",
        ] {
            assert_eq!(Ja4::parse_forwarded(bad), None, "{bad}");
        }
    }
}
