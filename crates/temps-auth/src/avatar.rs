// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use base64::{engine::general_purpose, Engine as _};

const PALETTE: [(u8, u8, u8); 16] = [
    (0x1f, 0x77, 0xb4),
    (0xff, 0x7f, 0x0e),
    (0x2c, 0xa0, 0x2c),
    (0xd6, 0x27, 0x28),
    (0x94, 0x67, 0xbd),
    (0x8c, 0x56, 0x4b),
    (0xe3, 0x77, 0xc2),
    (0x7f, 0x7f, 0x7f),
    (0xbc, 0xbd, 0x22),
    (0x17, 0xbe, 0xcf),
    (0x4c, 0x72, 0xb0),
    (0xdd, 0x85, 0x52),
    (0x55, 0xa8, 0x68),
    (0xc4, 0x4e, 0x52),
    (0x81, 0x72, 0xb2),
    (0xcc, 0xb9, 0x74),
];

fn fnv1a(input: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in input.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn initials(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "?".to_string();
    }
    let mut letters: Vec<char> = trimmed
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .map(|c| c.to_uppercase().next().unwrap_or(c))
        .collect();
    if letters.len() == 1 {
        if let Some(second) = trimmed.chars().nth(1) {
            letters.push(second.to_uppercase().next().unwrap_or(second));
        }
    }
    letters.into_iter().collect()
}

fn pick_color(seed: u32) -> (u8, u8, u8) {
    PALETTE[(seed as usize) % PALETTE.len()]
}

/// Generates a deterministic SVG avatar as a `data:` URL.
///
/// The avatar contains the user's initials drawn on a background color
/// derived from a hash of the name, so the same name always renders the
/// same avatar. No network dependency, no third-party service.
pub fn generate_avatar_data_url(name: &str) -> String {
    let seed = fnv1a(name);
    let (r, g, b) = pick_color(seed);
    let text = initials(name);

    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128"><rect width="128" height="128" fill="#{r:02x}{g:02x}{b:02x}"/><text x="50%" y="50%" dy=".1em" fill="#ffffff" font-family="-apple-system,BlinkMacSystemFont,Segoe UI,Roboto,Helvetica,Arial,sans-serif" font-size="56" font-weight="600" text-anchor="middle" dominant-baseline="middle">{text}</text></svg>"##,
        r = r,
        g = g,
        b = b,
        text = text,
    );

    format!(
        "data:image/svg+xml;base64,{}",
        general_purpose::STANDARD.encode(svg.as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_same_name() {
        assert_eq!(
            generate_avatar_data_url("David Viejo"),
            generate_avatar_data_url("David Viejo"),
        );
    }

    #[test]
    fn different_names_get_different_avatars() {
        assert_ne!(
            generate_avatar_data_url("Alice"),
            generate_avatar_data_url("Bob"),
        );
    }

    #[test]
    fn produces_data_url() {
        let url = generate_avatar_data_url("Jane Doe");
        assert!(url.starts_with("data:image/svg+xml;base64,"));
    }

    #[test]
    fn initials_two_words() {
        assert_eq!(initials("David Viejo"), "DV");
    }

    #[test]
    fn initials_single_word_uses_two_chars() {
        assert_eq!(initials("alice"), "AL");
    }

    #[test]
    fn initials_empty_falls_back() {
        assert_eq!(initials("   "), "?");
    }

    #[test]
    fn initials_skips_extra_words() {
        assert_eq!(initials("John Ronald Reuel Tolkien"), "JR");
    }
}
