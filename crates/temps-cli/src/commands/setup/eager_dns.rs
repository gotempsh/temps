// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/// A serving certificate can be reused. Only a new DNS-01 order needs a contact.
pub(super) fn issuance_contact(
    reusable: bool,
    contact: Option<&str>,
) -> anyhow::Result<Option<&str>> {
    if reusable {
        return Ok(None);
    }
    let email = contact.ok_or_else(|| anyhow::anyhow!(
        "Eager DNS-01 issuance requires a deliverable ACME contact. Pass --letsencrypt-email or --email."
    ))?;
    Ok(Some(email))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_stored_serving_certificate_skips_the_acme_contact() {
        assert_eq!(issuance_contact(true, None).unwrap(), None);
        assert_eq!(
            issuance_contact(true, Some("certs@example.com")).unwrap(),
            None
        );
        assert!(issuance_contact(false, None).is_err());
        assert_eq!(
            issuance_contact(false, Some("certs@example.com")).unwrap(),
            Some("certs@example.com")
        );
    }
}
