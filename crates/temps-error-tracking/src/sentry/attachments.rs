// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Attachment items of a Sentry envelope (ADR 111, D4).
//!
//! An attachment never rejects its envelope: one that is not a plain file,
//! is too large, or has no event or feedback to belong to is reported as
//! unsupported and the rest of the envelope is stored as usual.

use axum::body::Bytes;

use super::envelope::{ItemHeader, ItemType};
use crate::services::attachments::{
    clean_content_type, clean_filename, normalize_event_id, IncomingAttachment,
    MAX_ATTACHMENTS_PER_EVENT, MAX_ATTACHMENT_BYTES, MAX_EVENT_ATTACHMENT_BYTES,
    STORED_ATTACHMENT_TYPE,
};

/// The attachments kept from one envelope, and the SDK event id (32
/// lowercase hex digits) of the event or feedback they belong to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAttachments {
    pub sdk_event_id: String,
    pub items: Vec<IncomingAttachment>,
}

/// Collects the attachment items of an envelope while it is read.
#[derive(Debug, Default)]
pub struct AttachmentCollector {
    items: Vec<IncomingAttachment>,
    bytes: usize,
}

impl AttachmentCollector {
    /// Keep `payload` if it is a plain file within the limits. Returns
    /// whether it was kept; one that is not counts as unsupported.
    pub fn push(&mut self, header: &ItemHeader, payload: &[u8]) -> bool {
        if header.ty != ItemType::Attachment {
            return false;
        }
        let text = |key: &str| header.other.get(key).and_then(|value| value.as_str());
        // Absent means a plain file; present but not a string is not one.
        let attachment_type = match header.other.get("attachment_type") {
            None => STORED_ATTACHMENT_TYPE,
            Some(value) => match value.as_str() {
                Some(kind) => kind,
                None => return false,
            },
        };
        if attachment_type != STORED_ATTACHMENT_TYPE
            || payload.len() > MAX_ATTACHMENT_BYTES
            || self.items.len() >= MAX_ATTACHMENTS_PER_EVENT
            || self.bytes + payload.len() > MAX_EVENT_ATTACHMENT_BYTES
        {
            return false;
        }
        self.bytes += payload.len();
        self.items.push(IncomingAttachment {
            filename: clean_filename(text("filename")),
            content_type: clean_content_type(header.content_type.as_deref()),
            attachment_type: attachment_type.to_string(),
            body: Bytes::copy_from_slice(payload),
        });
        true
    }

    /// The kept attachments and who they belong to: the envelope's event id,
    /// else the id of its only event, else that of its only feedback. `Err`
    /// carries how many were kept but have no owner.
    pub fn finish(
        self,
        envelope_event_id: Option<&str>,
        event_ids: &[&str],
        feedback_ids: &[&str],
    ) -> Result<Option<ParsedAttachments>, usize> {
        if self.items.is_empty() {
            return Ok(None);
        }
        let owner = envelope_event_id
            .or(match event_ids {
                [only] => Some(*only),
                _ => None,
            })
            .or(match feedback_ids {
                [only] => Some(*only),
                _ => None,
            })
            .and_then(normalize_event_id);
        match owner {
            Some(sdk_event_id) => Ok(Some(ParsedAttachments {
                sdk_event_id,
                items: self.items,
            })),
            None => Err(self.items.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const EVENT: &str = "0123456789abcdef0123456789abcdef";

    fn header(extra: &[(&str, &str)], content_type: Option<&str>) -> ItemHeader {
        ItemHeader {
            ty: ItemType::Attachment,
            length: None,
            content_type: content_type.map(str::to_string),
            item_count: None,
            other: extra
                .iter()
                .map(|(key, value)| (key.to_string(), serde_json::json!(value)))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn keeps_a_plain_file_with_a_clean_name_and_type() {
        let mut collector = AttachmentCollector::default();
        assert!(collector.push(
            &header(
                &[("filename", "logs/app.log")],
                Some("Text/Plain; charset=utf-8")
            ),
            b"line one\n",
        ));
        let parsed = collector
            .finish(Some(EVENT), &[], &[])
            .expect("the envelope has an event id")
            .expect("one attachment");
        assert_eq!(parsed.sdk_event_id, EVENT);
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].filename, "app.log");
        assert_eq!(parsed.items[0].content_type, "text/plain");
        assert_eq!(parsed.items[0].attachment_type, "event.attachment");
        assert_eq!(&parsed.items[0].body[..], b"line one\n");
    }

    #[test]
    fn native_crash_attachments_are_not_kept() {
        let mut collector = AttachmentCollector::default();
        for kind in ["event.minidump", "event.applecrashreport", "unreal.context"] {
            assert!(!collector.push(&header(&[("attachment_type", kind)], None), b"MDMP"));
        }
        assert_eq!(collector.finish(Some(EVENT), &[], &[]), Ok(None));
    }

    #[test]
    fn an_attachment_type_that_is_not_a_string_is_not_kept() {
        let mut collector = AttachmentCollector::default();
        for value in [
            serde_json::json!(null),
            serde_json::json!(7),
            serde_json::json!(["event.attachment"]),
        ] {
            let mut item = header(&[], None);
            item.other.insert("attachment_type".to_string(), value);
            assert!(!collector.push(&item, b"data"));
        }
        assert_eq!(collector.finish(Some(EVENT), &[], &[]), Ok(None));
    }

    #[test]
    fn enforces_the_size_and_count_limits() {
        let mut collector = AttachmentCollector::default();
        let too_large = vec![0u8; MAX_ATTACHMENT_BYTES + 1];
        assert!(!collector.push(&header(&[], None), &too_large));
        let largest = vec![0u8; MAX_ATTACHMENT_BYTES];
        assert!(collector.push(&header(&[], None), &largest));
        assert!(collector.push(&header(&[], None), &largest));
        // 4 MiB for the event are used up.
        assert!(!collector.push(&header(&[], None), b"x"));

        let mut many = AttachmentCollector::default();
        for _ in 0..MAX_ATTACHMENTS_PER_EVENT {
            assert!(many.push(&header(&[], None), b"x"));
        }
        assert!(!many.push(&header(&[], None), b"x"));
    }

    #[test]
    fn the_owner_is_the_envelope_event_then_the_only_event_then_the_only_feedback() {
        let other = "fedcba9876543210fedcba9876543210";
        let collect = || {
            let mut collector = AttachmentCollector::default();
            collector.push(&header(&[], None), b"x");
            collector
        };
        let owner = |result: Result<Option<ParsedAttachments>, usize>| {
            result.ok().flatten().map(|parsed| parsed.sdk_event_id)
        };
        assert_eq!(
            owner(collect().finish(Some(EVENT), &[other], &[])).as_deref(),
            Some(EVENT)
        );
        assert_eq!(
            owner(collect().finish(None, &[other], &[EVENT])).as_deref(),
            Some(other)
        );
        assert_eq!(
            owner(collect().finish(None, &[], &[other])).as_deref(),
            Some(other)
        );
        // Two events and no envelope id: whose they are is unknown.
        assert_eq!(collect().finish(None, &[EVENT, other], &[]), Err(1));
        assert_eq!(collect().finish(None, &[], &[]), Err(1));
        assert_eq!(collect().finish(Some("not-hex"), &[], &[]), Err(1));
    }
}
