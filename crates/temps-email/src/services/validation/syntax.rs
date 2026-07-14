//! Email address syntax validation.
//!
//! Pragmatic RFC 5321/5322 parsing — strict enough to reject the addresses
//! that always bounce, lenient enough not to reject deliverable ones. We do
//! not attempt full RFC 5322 (quoted local-parts, comments) because such
//! addresses are vanishingly rare and SMTP probing catches the rest.

/// Outcome of parsing an email address into its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEmail {
    pub local_part: String,
    pub domain: String,
}

/// Validate the syntax of an email address and split it into local-part and
/// domain. Returns `None` if the address is not syntactically valid.
pub fn parse_email(email: &str) -> Option<ParsedEmail> {
    let email = email.trim();
    if email.len() > 254 {
        return None;
    }

    // Exactly one '@', and it must not be at either end.
    let at = email.find('@')?;
    if email[at + 1..].contains('@') {
        return None;
    }
    let local = &email[..at];
    let domain = &email[at + 1..];

    if !is_valid_local_part(local) || !is_valid_domain(domain) {
        return None;
    }

    Some(ParsedEmail {
        local_part: local.to_string(),
        domain: domain.to_string(),
    })
}

/// Validate the local-part (the bit before `@`). Total address length and
/// local-part length limits come from RFC 5321 §4.5.3.1.
fn is_valid_local_part(local: &str) -> bool {
    if local.is_empty() || local.len() > 64 {
        return false;
    }
    // Dot-atom: cannot start/end with a dot, no consecutive dots.
    if local.starts_with('.') || local.ends_with('.') || local.contains("..") {
        return false;
    }
    // Permitted unquoted local-part characters (RFC 5322 atext + '.').
    local.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '.' | '!'
                    | '#'
                    | '$'
                    | '%'
                    | '&'
                    | '\''
                    | '*'
                    | '+'
                    | '-'
                    | '/'
                    | '='
                    | '?'
                    | '^'
                    | '_'
                    | '`'
                    | '{'
                    | '|'
                    | '}'
                    | '~'
            )
    })
}

/// Validate the domain part. We accept conventional DNS host names; an MX
/// lookup later decides whether the domain actually accepts mail.
fn is_valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 {
        return false;
    }
    // A deliverable domain has at least one dot (TLD); reject bare hostnames.
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return false;
    }
    domain.split('.').all(is_valid_label)
}

fn is_valid_label(label: &str) -> bool {
    if label.is_empty() || label.len() > 63 {
        return false;
    }
    if label.starts_with('-') || label.ends_with('-') {
        return false;
    }
    label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Common typo domains mapped to their intended correction. Used to offer a
/// "did you mean …" suggestion when the address is otherwise valid.
const DOMAIN_TYPOS: &[(&str, &str)] = &[
    ("gmial.com", "gmail.com"),
    ("gmai.com", "gmail.com"),
    ("gmal.com", "gmail.com"),
    ("gnail.com", "gmail.com"),
    ("gmail.co", "gmail.com"),
    ("gmail.cm", "gmail.com"),
    ("hotmial.com", "hotmail.com"),
    ("hotmai.com", "hotmail.com"),
    ("hotmal.com", "hotmail.com"),
    ("hotmail.co", "hotmail.com"),
    ("yaho.com", "yahoo.com"),
    ("yahooo.com", "yahoo.com"),
    ("yahoo.co", "yahoo.com"),
    ("outlok.com", "outlook.com"),
    ("outloo.com", "outlook.com"),
    ("iclould.com", "icloud.com"),
    ("icloud.co", "icloud.com"),
];

/// Suggest a corrected address if the domain looks like a known typo.
pub fn suggest_correction(parsed: &ParsedEmail) -> Option<String> {
    let domain_lower = parsed.domain.to_ascii_lowercase();
    DOMAIN_TYPOS
        .iter()
        .find(|(typo, _)| *typo == domain_lower)
        .map(|(_, correct)| format!("{}@{}", parsed.local_part, correct))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_addresses() {
        for email in [
            "test@example.com",
            "user.name@example.com",
            "user+tag@sub.example.co.uk",
            "first.last@example.com",
            "x@y.io",
            "user_name@example-domain.com",
        ] {
            assert!(parse_email(email).is_some(), "{email} should be valid");
        }
    }

    #[test]
    fn test_invalid_addresses() {
        for email in [
            "",
            "plainaddress",
            "@example.com",
            "user@",
            "user@@example.com",
            "user@nodot",
            ".user@example.com",
            "user.@example.com",
            "user..name@example.com",
            "user@example..com",
            "user@-example.com",
            "user@example-.com",
            "user name@example.com",
        ] {
            assert!(parse_email(email).is_none(), "{email} should be invalid");
        }
    }

    #[test]
    fn test_parts_extracted() {
        let p = parse_email("alice.smith@mail.example.com").unwrap();
        assert_eq!(p.local_part, "alice.smith");
        assert_eq!(p.domain, "mail.example.com");
    }

    #[test]
    fn test_trims_whitespace() {
        assert!(parse_email("  test@example.com  ").is_some());
    }

    #[test]
    fn test_typo_suggestion() {
        let p = parse_email("john@gmial.com").unwrap();
        assert_eq!(suggest_correction(&p), Some("john@gmail.com".to_string()));

        let ok = parse_email("john@gmail.com").unwrap();
        assert_eq!(suggest_correction(&ok), None);
    }

    #[test]
    fn test_local_part_length_limit() {
        let long_local = "a".repeat(65);
        assert!(parse_email(&format!("{long_local}@example.com")).is_none());
        let ok_local = "a".repeat(64);
        assert!(parse_email(&format!("{ok_local}@example.com")).is_some());
    }

    #[test]
    fn test_total_address_length_limit() {
        let long_domain = format!(
            "{}.{}.{}.com",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63)
        );
        let email = format!("{}@{long_domain}", "d".repeat(64));
        assert!(email.len() > 254);
        assert!(parse_email(&email).is_none());
    }
}
