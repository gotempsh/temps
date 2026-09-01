// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Miscellaneous email signals: disposable-provider, role-account, and
//! B2C-provider detection, plus the Gravatar profile-image URL.
//!
//! The lists here are deliberately small and high-signal — they cover the
//! providers that actually move the needle for deliverability decisions
//! rather than attempting an exhaustive catalogue.

/// A non-exhaustive list of well-known disposable / throwaway email domains.
const DISPOSABLE_DOMAINS: &[&str] = &[
    "10minutemail.com",
    "guerrillamail.com",
    "guerrillamail.net",
    "mailinator.com",
    "tempmail.com",
    "temp-mail.org",
    "throwawaymail.com",
    "yopmail.com",
    "trashmail.com",
    "getnada.com",
    "maildrop.cc",
    "dispostable.com",
    "fakeinbox.com",
    "sharklasers.com",
    "spam4.me",
    "mailnesia.com",
    "mintemail.com",
    "mohmal.com",
    "emailondeck.com",
    "tempinbox.com",
];

/// Local-parts that denote a shared / role mailbox rather than a person.
const ROLE_LOCAL_PARTS: &[&str] = &[
    "admin",
    "administrator",
    "billing",
    "contact",
    "help",
    "hello",
    "hostmaster",
    "info",
    "mail",
    "marketing",
    "noc",
    "no-reply",
    "noreply",
    "office",
    "postmaster",
    "root",
    "sales",
    "security",
    "support",
    "sysadmin",
    "team",
    "webmaster",
    "abuse",
    "privacy",
    "legal",
];

/// Consumer (B2C) mailbox providers — a free personal address rather than a
/// company domain.
const B2C_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "yahoo.com",
    "yahoo.co.uk",
    "ymail.com",
    "hotmail.com",
    "hotmail.co.uk",
    "outlook.com",
    "live.com",
    "msn.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "protonmail.com",
    "proton.me",
    "gmx.com",
    "gmx.net",
    "mail.com",
    "zoho.com",
    "yandex.com",
];

/// Whether the domain is a known disposable / throwaway provider.
pub fn is_disposable(domain: &str) -> bool {
    let d = domain.to_ascii_lowercase();
    DISPOSABLE_DOMAINS.contains(&d.as_str())
}

/// Whether the local-part denotes a role / shared mailbox (e.g. `info@`).
pub fn is_role_account(local_part: &str) -> bool {
    let l = local_part.to_ascii_lowercase();
    ROLE_LOCAL_PARTS.contains(&l.as_str())
}

/// Whether the domain is a known consumer (B2C) mailbox provider.
pub fn is_b2c(domain: &str) -> bool {
    let d = domain.to_ascii_lowercase();
    B2C_DOMAINS.contains(&d.as_str())
}

/// Build the Gravatar profile-image URL for an address. Gravatar keys on the
/// MD5 of the lowercased, trimmed address; `d=404` makes the URL 404 when no
/// avatar exists so callers can probe for presence.
pub fn gravatar_url(email: &str) -> String {
    let normalized = email.trim().to_ascii_lowercase();
    let digest = md5::compute(normalized.as_bytes());
    format!("https://www.gravatar.com/avatar/{digest:x}?d=404")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disposable() {
        assert!(is_disposable("mailinator.com"));
        assert!(is_disposable("Guerrillamail.com")); // case-insensitive
        assert!(!is_disposable("gmail.com"));
        assert!(!is_disposable("acme-corp.com"));
    }

    #[test]
    fn test_role_account() {
        assert!(is_role_account("info"));
        assert!(is_role_account("ADMIN"));
        assert!(is_role_account("no-reply"));
        assert!(!is_role_account("john.smith"));
        assert!(!is_role_account("alice"));
    }

    #[test]
    fn test_b2c() {
        assert!(is_b2c("gmail.com"));
        assert!(is_b2c("Outlook.com"));
        assert!(!is_b2c("acme-corp.com"));
    }

    #[test]
    fn test_gravatar_url() {
        // Known MD5 of "test@example.com".
        let url = gravatar_url("test@example.com");
        assert!(url.starts_with("https://www.gravatar.com/avatar/"));
        assert!(url.ends_with("?d=404"));
        // Normalization: case and surrounding whitespace must not matter.
        assert_eq!(gravatar_url("  Test@Example.COM "), url);
    }
}
