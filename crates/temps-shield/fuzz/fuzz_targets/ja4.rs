// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whatever a client sends first, the JA4 parser answers without panicking,
//! and never claims more bytes than it was given.

#![no_main]

use libfuzzer_sys::fuzz_target;
use temps_shield::ja4::{self, Need, Transport};

fuzz_target!(|data: &[u8]| {
    if let Ok(Need::Done(end)) = ja4::needed(data) {
        assert!(end <= data.len());
        assert!(end <= ja4::MAX_CLIENT_HELLO);
    }
    if let Ok(hello) = ja4::parse(data) {
        let fingerprint = ja4::ja4(&hello, Transport::Tcp);
        assert_eq!(fingerprint.as_str().len(), 36);
        let _ = ja4::ja4_raw(&hello, Transport::Tcp);
    }
});
