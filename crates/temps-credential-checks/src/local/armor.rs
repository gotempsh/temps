// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Splits text into `-----BEGIN <label>-----` blocks (PEM and OpenPGP armor).
//! A block's body is only decoded by the reader that understands its label, so
//! an unrelated block (an encrypted private key with RFC 1421 headers, say)
//! never makes the rest of the value unreadable.

pub(super) struct Block<'a> {
    pub label: &'a str,
    /// Base64 body lines joined, with headers and blank lines removed.
    pub body: String,
    /// OpenPGP armor checksum (`=XXXX`), when present.
    pub checksum: Option<&'a str>,
    /// False when the text ended before `-----END <label>-----`.
    pub terminated: bool,
}

pub(super) fn blocks(text: &str) -> Vec<Block<'_>> {
    let mut blocks = Vec::new();
    let mut lines = text.lines().map(str::trim);
    while let Some(line) = lines.next() {
        let Some(label) = line
            .strip_prefix("-----BEGIN ")
            .and_then(|rest| rest.strip_suffix("-----"))
        else {
            continue;
        };
        let end = format!("-----END {label}-----");
        let mut block = Block {
            label,
            body: String::new(),
            checksum: None,
            terminated: false,
        };
        for line in lines.by_ref() {
            if line == end {
                block.terminated = true;
                break;
            }
            // Base64 never contains ':', so these are RFC 1421 or armor headers.
            if line.is_empty() || line.contains(':') {
                continue;
            }
            if let Some(checksum) = line.strip_prefix('=').filter(|c| c.len() == 4) {
                block.checksum = Some(checksum);
                continue;
            }
            block.body.push_str(line);
        }
        blocks.push(block);
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_blocks_and_skips_headers() {
        let text = "intro\n  -----BEGIN RSA PRIVATE KEY-----\n  Proc-Type: 4,ENCRYPTED\n  DEK-Info: AES-128-CBC,00\n\n  QUJD\n  -----END RSA PRIVATE KEY-----\n-----BEGIN PGP PUBLIC KEY BLOCK-----\nComment: test\n\nREVG\nR0g=\n=AbCd\n-----END PGP PUBLIC KEY BLOCK-----\n-----BEGIN CERTIFICATE-----\nSUpL";
        let blocks = blocks(text);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].label, "RSA PRIVATE KEY");
        assert_eq!(blocks[0].body, "QUJD");
        assert!(blocks[0].terminated);
        assert_eq!(blocks[1].label, "PGP PUBLIC KEY BLOCK");
        assert_eq!(blocks[1].body, "REVGR0g=");
        assert_eq!(blocks[1].checksum, Some("AbCd"));
        assert_eq!(blocks[2].label, "CERTIFICATE");
        assert!(!blocks[2].terminated);
    }
}
