// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A labelled image listing read as a stream, holding at most one page.
//!
//! Docker cannot limit or page `GET /images/json`, and bollard's typed
//! `list_images` reads the whole answer into a `Vec` before the caller sees
//! any of it. So this asks for the listing through the caller's own client
//! (whatever its transport: Unix socket, TCP, TLS), takes the answer as the
//! raw byte stream bollard hands out for streaming endpoints, splits it into
//! its array entries as the bytes arrive, and keeps only the page: the
//! oldest `max` images after the cursor, in a heap of that size. What it
//! holds is one page plus one entry, however many images the daemon has.
//!
//! The request is built as bollard's streaming `GET /images/{name}/get`
//! for a placeholder name and rewritten, by a request modifier on a clone of
//! the caller's client, into `GET /images/json?filters=…` before it is
//! sent. If the rewrite ever stopped matching, the daemon would be asked to
//! export an image that does not exist and answer 404: an error, never a
//! wrong listing.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use bollard::Docker;
use futures::StreamExt as _;
use serde::Deserialize;

use super::{LabelledImage, LabelledImages, ListCursor};
use crate::DockerUtilError;

/// The placeholder image name the listing request is built with, then
/// rewritten from.
const LISTING_PLACEHOLDER: &str = "temps-labelled-image-listing";

/// Largest single entry of the listing held while it is read. An image's
/// entry is its IDs, tags, digests and labels: a few KiB in practice.
pub(super) const MAX_LISTING_ENTRY_BYTES: usize = 1024 * 1024;

/// The fields of one listing entry the page needs; the rest are skipped.
#[derive(Deserialize)]
struct ListedImage {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "RepoTags", default)]
    repo_tags: Option<Vec<String>>,
    #[serde(rename = "Labels", default)]
    labels: Option<HashMap<String, String>>,
    #[serde(rename = "Created")]
    created: i64,
}

/// An image kept on the page, ordered by `(created, image_id)` so the heap's
/// top is the newest kept one: the first to give way to an older image.
struct Kept(LabelledImage);

impl Kept {
    fn position(&self) -> (i64, &str) {
        (self.0.created, self.0.image_id.as_str())
    }
}

impl PartialEq for Kept {
    fn eq(&self, other: &Self) -> bool {
        self.position() == other.position()
    }
}

impl Eq for Kept {}

impl PartialOrd for Kept {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Kept {
    fn cmp(&self, other: &Self) -> Ordering {
        self.position().cmp(&other.position())
    }
}

/// The oldest `max` images after a cursor, from images offered one at a
/// time in any order, holding no more than `max` of them.
pub(super) struct PageCollector<'a> {
    after: Option<&'a ListCursor>,
    max: usize,
    kept: BinaryHeap<Kept>,
    unlisted: usize,
}

impl<'a> PageCollector<'a> {
    pub(super) fn new(after: Option<&'a ListCursor>, max: usize) -> Self {
        Self {
            after,
            max,
            kept: BinaryHeap::with_capacity(max.min(1024)),
            unlisted: 0,
        }
    }

    /// Whether an image at `position` belongs after the cursor.
    pub(super) fn wants(&self, position: (i64, &str)) -> bool {
        self.after
            .is_none_or(|after| position > (after.created, after.image_id.as_str()))
    }

    /// Offer one image; it is kept if it is among the oldest `max` so far.
    pub(super) fn offer(&mut self, image: LabelledImage) {
        let offered = Kept(image);
        if !self.wants(offered.position()) {
            return;
        }
        if self.kept.len() < self.max {
            self.kept.push(offered);
            return;
        }
        self.unlisted += 1;
        if self
            .kept
            .peek()
            .is_some_and(|newest| offered.position() < newest.position())
        {
            self.kept.pop();
            self.kept.push(offered);
        }
    }

    /// The page, oldest first, with where the next one starts.
    pub(super) fn finish(self) -> LabelledImages {
        let images: Vec<LabelledImage> = self
            .kept
            .into_sorted_vec()
            .into_iter()
            .map(|kept| kept.0)
            .collect();
        let next = (self.unlisted > 0)
            .then(|| images.last())
            .flatten()
            .map(|last| ListCursor {
                created: last.created,
                image_id: last.image_id.clone(),
            });
        LabelledImages {
            images,
            unlisted: self.unlisted,
            next,
        }
    }
}

/// Splits a JSON array arriving in chunks into its top-level entries,
/// holding one entry at a time.
#[derive(Default)]
pub(super) struct ArraySplitter {
    entry: Vec<u8>,
    entry_start: u64,
    offset: u64,
    depth: u32,
    in_string: bool,
    escaped: bool,
    opened: bool,
    closed: bool,
}

impl ArraySplitter {
    /// Feed the next chunk, calling `on_entry` with each complete entry.
    pub(super) fn feed(
        &mut self,
        chunk: &[u8],
        mut on_entry: impl FnMut(u64, &[u8]) -> Result<(), DockerUtilError>,
    ) -> Result<(), DockerUtilError> {
        for &byte in chunk {
            let at = self.offset;
            self.offset += 1;
            if self.closed {
                if !byte.is_ascii_whitespace() {
                    return Err(malformed(at, "data after the end of the array"));
                }
                continue;
            }
            if self.depth == 0 {
                match byte {
                    b if b.is_ascii_whitespace() => {}
                    b'[' if !self.opened => self.opened = true,
                    b',' if self.opened => {}
                    b']' if self.opened => self.closed = true,
                    b'{' if self.opened => {
                        self.depth = 1;
                        self.entry_start = at;
                        self.entry.push(byte);
                    }
                    _ if !self.opened => return Err(malformed(at, "it does not start with '['")),
                    _ => return Err(malformed(at, "an entry is not a JSON object")),
                }
                continue;
            }
            if self.entry.len() >= MAX_LISTING_ENTRY_BYTES {
                return Err(DockerUtilError::ImageListingEntryTooLarge {
                    offset: self.entry_start,
                    max_bytes: MAX_LISTING_ENTRY_BYTES,
                });
            }
            self.entry.push(byte);
            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                } else if byte == b'\\' {
                    self.escaped = true;
                } else if byte == b'"' {
                    self.in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => self.in_string = true,
                b'{' | b'[' => self.depth += 1,
                b'}' | b']' => {
                    self.depth -= 1;
                    if self.depth == 0 {
                        on_entry(self.entry_start, &self.entry)?;
                        self.entry.clear();
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Check the array was complete when the stream ended.
    pub(super) fn finish(&self) -> Result<(), DockerUtilError> {
        if self.closed {
            Ok(())
        } else {
            Err(malformed(
                self.offset,
                "the answer ended before the array did",
            ))
        }
    }
}

fn malformed(offset: u64, reason: &'static str) -> DockerUtilError {
    DockerUtilError::ImageListingMalformed { offset, reason }
}

/// `filters=<the JSON filter map, percent-encoded>` asking for images that
/// carry every `key=value` of `labels`.
fn filters_query(labels: &[(&str, &str)]) -> String {
    let wanted: Vec<String> = labels
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let filters = serde_json::json!({ "label": wanted }).to_string();
    format!(
        "filters={}",
        percent_encoding::utf8_percent_encode(&filters, percent_encoding::NON_ALPHANUMERIC)
    )
}

/// A clone of `docker` whose placeholder export request becomes the
/// labelled listing.
fn listing_client(docker: &Docker, query: String) -> Docker {
    let placeholder = format!("/images/{LISTING_PLACEHOLDER}/get");
    docker.clone().with_request_modifier(move |mut request| {
        let uri = request.uri().clone();
        let Some(prefix) = uri.path().strip_suffix(placeholder.as_str()) else {
            return request;
        };
        let mut parts = uri.clone().into_parts();
        let Ok(path_and_query) = format!("{prefix}/images/json?{query}").parse() else {
            return request;
        };
        parts.path_and_query = Some(path_and_query);
        if let Ok(rewritten) = http::Uri::from_parts(parts) {
            *request.uri_mut() = rewritten;
        }
        request
    })
}

/// Images carrying every `key=value` of `labels`, read as a stream into a
/// page of at most `max` after `after` (see the module documentation).
pub(super) async fn stream_labelled_images(
    docker: &Docker,
    labels: &[(&str, &str)],
    keep_prefix: &str,
    after: Option<&ListCursor>,
    max: usize,
) -> Result<LabelledImages, DockerUtilError> {
    let client = listing_client(docker, filters_query(labels));
    let mut body = client.export_image(LISTING_PLACEHOLDER);
    let mut splitter = ArraySplitter::default();
    let mut page = PageCollector::new(after, max);
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|source| DockerUtilError::ImageInventory {
            operation: "list labelled images",
            source,
        })?;
        splitter.feed(&chunk, |offset, entry| {
            let listed: ListedImage = serde_json::from_slice(entry)
                .map_err(|source| DockerUtilError::ImageListingEntry { offset, source })?;
            if page.wants((listed.created, listed.id.as_str())) {
                page.offer(labelled(listed, keep_prefix));
            }
            Ok(())
        })?;
    }
    splitter.finish()?;
    Ok(page.finish())
}

fn labelled(image: ListedImage, keep_prefix: &str) -> LabelledImage {
    LabelledImage {
        image_id: image.id,
        tags: image
            .repo_tags
            .unwrap_or_default()
            .into_iter()
            .filter(|tag| tag != "<none>:<none>")
            .collect(),
        labels: image
            .labels
            .unwrap_or_default()
            .into_iter()
            .filter(|(label, _)| label.starts_with(keep_prefix))
            .collect(),
        created: image.created,
        in_use_by: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(chunks: &[&[u8]]) -> Result<Vec<String>, DockerUtilError> {
        let mut splitter = ArraySplitter::default();
        let mut found = Vec::new();
        for chunk in chunks {
            splitter.feed(chunk, |_, entry| {
                found.push(String::from_utf8_lossy(entry).into_owned());
                Ok(())
            })?;
        }
        splitter.finish()?;
        Ok(found)
    }

    /// Entries are split at their own closing brace, however the bytes are
    /// chunked, with braces, brackets and escaped quotes inside strings and
    /// nested objects inside an entry left alone.
    #[test]
    fn the_array_is_split_into_its_entries_whatever_the_chunking() {
        let body = br#" [ {"Id":"a","Labels":{"k":"v}"},"RepoTags":["x:1"]},
            {"Id":"b \"]{","Labels":null,"Created":2} ,{"Id":"c","Nested":[[{}]]} ] "#;
        let whole = entries(&[body]).expect("whole");
        let bytewise: Vec<&[u8]> = body.chunks(1).collect();
        assert_eq!(entries(&bytewise).expect("byte by byte"), whole);
        assert_eq!(whole.len(), 3, "{whole:?}");
        assert!(whole[1].contains(r#"b \"]{"#), "{whole:?}");
        assert_eq!(entries(&[b"[]"]).expect("empty"), Vec::<String>::new());
    }

    #[test]
    fn an_answer_that_is_not_a_complete_array_is_refused() {
        for (body, reason) in [
            (&br#"{"message":"x"}"#[..], "it does not start with '['"),
            (
                &br#"[{"Id":"a"}"#[..],
                "the answer ended before the array did",
            ),
            (&br#"[1]"#[..], "an entry is not a JSON object"),
            (&br#"[] []"#[..], "data after the end of the array"),
        ] {
            let error = entries(&[body]).expect_err("refused");
            assert!(
                matches!(&error, DockerUtilError::ImageListingMalformed { reason: found, .. } if *found == reason),
                "{error:?}"
            );
        }
    }

    #[test]
    fn an_entry_larger_than_the_reader_holds_is_refused() {
        let mut splitter = ArraySplitter::default();
        let mut body = b"[{\"Id\":\"".to_vec();
        body.extend(std::iter::repeat_n(b'x', MAX_LISTING_ENTRY_BYTES));
        let error = splitter.feed(&body, |_, _| Ok(())).expect_err("too large");
        assert!(
            matches!(
                error,
                DockerUtilError::ImageListingEntryTooLarge { offset: 1, .. }
            ),
            "{error:?}"
        );
    }

    fn image(id: &str, created: i64) -> LabelledImage {
        LabelledImage {
            image_id: id.to_string(),
            tags: Vec::new(),
            labels: HashMap::new(),
            created,
            in_use_by: None,
        }
    }

    /// However many images are offered, in whatever order, the collector
    /// holds at most `max` and keeps the oldest after the cursor.
    #[test]
    fn the_page_holds_at_most_max_and_keeps_the_oldest() {
        let after = ListCursor {
            created: 10,
            image_id: "img-0010".to_string(),
        };
        let mut page = PageCollector::new(Some(&after), 3);
        for n in (0..5_000).rev() {
            page.offer(image(&format!("img-{n:04}"), n));
            assert!(page.kept.len() <= 3);
        }
        let page = page.finish();
        let ids: Vec<&str> = page.images.iter().map(|i| i.image_id.as_str()).collect();
        assert_eq!(ids, ["img-0011", "img-0012", "img-0013"]);
        assert_eq!(page.unlisted, 5_000 - 11 - 3);
        assert_eq!(
            page.next,
            Some(ListCursor {
                created: 13,
                image_id: "img-0013".to_string()
            })
        );
    }

    /// The listing filter is the JSON label map Docker documents, encoded
    /// so it survives the query string.
    #[test]
    fn the_filter_asks_for_every_label() {
        let query = filters_query(&[("sh.temps.image.instance", "a.b"), ("p", "7")]);
        let encoded = query.strip_prefix("filters=").expect("one parameter");
        let decoded = percent_encoding::percent_decode_str(encoded)
            .decode_utf8()
            .expect("utf-8");
        let filters: serde_json::Value = serde_json::from_str(&decoded).expect("JSON");
        assert_eq!(
            filters,
            serde_json::json!({ "label": ["sh.temps.image.instance=a.b", "p=7"] })
        );
        assert!(!encoded.contains(['{', '"', '=', '&', ' ']), "{encoded}");
    }

    /// Over a TCP transport the rewritten request reaches `GET /images/json`
    /// with the filter, and the streamed answer is paged.
    #[tokio::test]
    async fn the_listing_is_asked_for_and_streamed_over_http() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub daemon");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).await.expect("read");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            let body = r#"[{"Id":"sha256:bb","RepoTags":["b:1"],"Labels":{"sh.temps.image.p":"7","other":"x"},"Created":2},{"Id":"sha256:aa","RepoTags":["<none>:<none>"],"Labels":null,"Created":1}]"#;
            // Chunked, as a daemon streams it.
            let mut response =
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n"
                    .to_string();
            for part in body.as_bytes().chunks(7) {
                response.push_str(&format!(
                    "{:x}\r\n{}\r\n",
                    part.len(),
                    String::from_utf8_lossy(part)
                ));
            }
            response.push_str("0\r\n\r\n");
            stream.write_all(response.as_bytes()).await.expect("write");
            String::from_utf8_lossy(&request).into_owned()
        });
        let docker = Docker::connect_with_http(
            &format!("http://{address}"),
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("client");

        let page =
            stream_labelled_images(&docker, &[("sh.temps.image.p", "7")], "sh.temps.", None, 1)
                .await
                .expect("listing");
        let request = server.await.expect("stub daemon");

        let first_line = request.lines().next().unwrap_or_default();
        assert!(
            first_line.starts_with("GET /") && first_line.contains("/images/json?filters="),
            "{first_line}"
        );
        assert!(!first_line.contains(LISTING_PLACEHOLDER), "{first_line}");
        assert_eq!(page.images.len(), 1, "{page:?}");
        assert_eq!(page.images[0].image_id, "sha256:aa", "the oldest is kept");
        assert!(page.images[0].tags.is_empty(), "untagged is no tag");
        assert_eq!(page.unlisted, 1);
        assert!(page.next.is_some());
        // The caller's client is left as it was.
        assert!(
            format!("{docker:?}").contains("request_modifier: None"),
            "{docker:?}"
        );
    }

    /// A daemon error is the daemon's, not a parse failure.
    #[tokio::test]
    async fn a_daemon_that_cannot_be_reached_fails_as_an_inventory_error() {
        let docker =
            Docker::connect_with_http("http://127.0.0.1:9", 5, bollard::API_DEFAULT_VERSION)
                .expect("client");
        let error = stream_labelled_images(&docker, &[("k", "v")], "sh.", None, 10)
            .await
            .expect_err("nothing listens on port 9");
        assert!(
            matches!(error, DockerUtilError::ImageInventory { .. }),
            "{error:?}"
        );
    }
}
