//! S3 utilities shared between backup engines and the orchestrator.

use anyhow::{anyhow, Result};

/// Sum the sizes of every object under `prefix` in `bucket`.
///
/// Used to compute size for backups that streamed straight to S3 (WAL-G,
/// rustfs migrate) — those engines don't see the bytes locally, so the
/// orchestrator lists the prefix after a successful run.
///
/// Returns the total in bytes. Empty prefixes return `Ok(0)`.
pub async fn list_total_size(
    s3_client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
) -> Result<i64> {
    let mut total: i64 = 0;
    let mut continuation_token: Option<String> = None;

    loop {
        let mut req = s3_client.list_objects_v2().bucket(bucket).prefix(prefix);
        if let Some(token) = continuation_token.take() {
            req = req.continuation_token(token);
        }

        let resp = req.send().await.map_err(|e| {
            anyhow!(
                "Failed to list S3 objects under s3://{}/{}: {}",
                bucket,
                prefix,
                e
            )
        })?;

        for obj in resp.contents() {
            total = total.saturating_add(obj.size().unwrap_or(0));
        }

        if resp.is_truncated() == Some(true) {
            continuation_token = resp.next_continuation_token().map(|s| s.to_string());
        } else {
            break;
        }
    }

    Ok(total)
}

/// Convert an `s3://bucket/key` URL into `(bucket, key)`. Returns `None` if
/// the URL is malformed or doesn't have the `s3://` scheme.
pub fn parse_s3_url(url: &str) -> Option<(String, String)> {
    let stripped = url.strip_prefix("s3://")?;
    let (bucket, key) = stripped.split_once('/')?;
    if bucket.is_empty() {
        return None;
    }
    Some((bucket.to_string(), key.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_s3_url_basic() {
        assert_eq!(
            parse_s3_url("s3://bucket/key/path"),
            Some(("bucket".to_string(), "key/path".to_string()))
        );
    }

    #[test]
    fn parse_s3_url_rejects_non_s3() {
        assert_eq!(parse_s3_url("https://example.com/foo"), None);
    }

    #[test]
    fn parse_s3_url_rejects_empty_bucket() {
        assert_eq!(parse_s3_url("s3:///key"), None);
    }

    #[test]
    fn parse_s3_url_no_key_returns_none() {
        // s3://bucket alone isn't useful — caller wants a key.
        assert_eq!(parse_s3_url("s3://bucket"), None);
    }
}
