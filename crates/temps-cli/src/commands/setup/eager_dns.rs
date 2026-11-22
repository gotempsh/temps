// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/// A serving certificate can be reused. Only a new DNS-01 order needs a contact.
pub(super) fn issuance_contact<'a>(
    status: &str,
    has_certificate: bool,
    contact: Option<&'a str>,
) -> anyhow::Result<Option<&'a str>> {
    if has_certificate && temps_entities::domains::CERT_SERVING_STATUSES.contains(&status) {
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
        for status in ["active", "active_renewal_failed"] {
            assert_eq!(issuance_contact(status, true, None).unwrap(), None);
            assert_eq!(
                issuance_contact(status, true, Some("certs@example.com")).unwrap(),
                None
            );
            assert!(issuance_contact(status, false, None).is_err());
        }
        assert!(issuance_contact("pending", true, None).is_err());
        assert_eq!(
            issuance_contact("pending", false, Some("certs@example.com")).unwrap(),
            Some("certs@example.com")
        );
    }
}
