// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! JA4 against an independent implementation: real ClientHellos and the
//! values Wireshark computed for them (`data/ja4_wireshark.txt`).

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
