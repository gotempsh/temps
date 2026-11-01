// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Markdown for Agents: HTML-to-Markdown conversion of upstream responses, and the compression and header handling around it.

use super::*;

/// Maximum HTML body size (in bytes) eligible for Markdown conversion.
/// Mirrors Cloudflare's "Markdown for Agents" 2 MB limit.
const MAX_MARKDOWN_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Estimate the number of tokens in a Markdown document using a simple
/// word-count heuristic (tokens ≈ words × 1.33, i.e. words / 0.75).
/// This matches the rough estimate used by the Cloudflare `x-markdown-tokens` header.
fn estimate_markdown_tokens(markdown: &str) -> usize {
    let word_count = markdown.split_whitespace().count();
    // 1 token ≈ 0.75 words  →  tokens ≈ words / 0.75 ≈ words * 4 / 3
    word_count * 4 / 3
}

/// Metadata extracted from a page's `<head>` for the YAML front-matter block.
struct PageMeta {
    title: Option<String>,
    description: Option<String>,
    image: Option<String>,
}

impl PageMeta {
    /// Return a YAML front-matter block, or `None` if no metadata was found.
    fn to_frontmatter(&self) -> Option<String> {
        if self.title.is_none() && self.description.is_none() && self.image.is_none() {
            return None;
        }
        let mut fm = String::from("---\n");
        if let Some(t) = &self.title {
            fm.push_str(&format!("title: {}\n", t));
        }
        if let Some(d) = &self.description {
            fm.push_str(&format!("description: {}\n", d));
        }
        if let Some(i) = &self.image {
            fm.push_str(&format!("image: {}\n", i));
        }
        fm.push_str("---\n\n");
        Some(fm)
    }
}

/// Parse YAML front-matter metadata from `<head>` meta tags.
///
/// Priority for `title`:
///   1. `<meta property="og:title">` — the short title without site-name suffix.
///   2. `<title>` — fallback, used when og:title is absent.
///
/// Priority for `description`:
///   1. `<meta name="description">` — canonical description.
///   2. `<meta property="og:description">` — fallback.
///
/// Priority for `image`:
///   1. `<meta property="image">` (Cloudflare convention).
///   2. `<meta property="og:image">`.
fn extract_page_meta(document: &scraper::Html) -> PageMeta {
    use scraper::Selector;

    // Helper: return the `content` attribute of the first element matching `sel`.
    let first_content = |sel: &str| -> Option<String> {
        Selector::parse(sel).ok().and_then(|s| {
            document
                .select(&s)
                .next()
                .and_then(|el| el.attr("content"))
                .map(|v| v.to_owned())
        })
    };

    // Title: prefer og:title (short), fall back to <title> text content.
    let title = first_content(r#"meta[property="og:title"]"#).or_else(|| {
        Selector::parse("title").ok().and_then(|s| {
            document
                .select(&s)
                .next()
                .map(|el| el.text().collect::<String>())
                .filter(|t| !t.is_empty())
        })
    });

    let description = first_content(r#"meta[name="description"]"#)
        .or_else(|| first_content(r#"meta[property="og:description"]"#));

    let image = first_content(r#"meta[property="image"]"#)
        .or_else(|| first_content(r#"meta[property="og:image"]"#));

    PageMeta {
        title,
        description,
        image,
    }
}

/// Extract the inner HTML of the content node to convert to Markdown.
///
/// Strategy (matches Cloudflare's Markdown for Agents behaviour):
/// 1. First `<main>` element found at shallowest depth (document order).
/// 2. Fall back to `<body>` if no `<main>` is present.
/// 3. Fall back to the full document string if neither is found (e.g. plain
///    HTML fragments without a body element).
///
/// `<script>` and `<style>` elements inside the selected node are stripped
/// before returning, preventing inline JS/CSS and JSON-LD blobs from appearing
/// as raw text in the converted Markdown.
///
/// Returns the cleaned inner HTML ready to feed to htmd.
fn extract_content_html(document: &scraper::Html) -> String {
    use scraper::Selector;

    let inner = {
        if let Ok(sel) = Selector::parse("main") {
            document.select(&sel).next().map(|node| node.inner_html())
        } else {
            None
        }
    }
    .or_else(|| {
        Selector::parse("body")
            .ok()
            .and_then(|sel| document.select(&sel).next().map(|node| node.inner_html()))
    })
    .unwrap_or_else(|| document.html());

    strip_script_and_style(&inner)
}

/// Remove all `<script>` and `<style>` tags (and their content) from an HTML
/// fragment string.  We re-parse the fragment through scraper so that nested
/// or malformed tags are handled correctly by the HTML5 parser.
fn strip_script_and_style(html: &str) -> String {
    use scraper::{Html, Selector};

    // Parse as a fragment so we don't add an implicit <html>/<body> wrapper.
    let fragment = Html::parse_fragment(html);
    // A literal selector; like `main`/`body` above, degrade rather than panic.
    let Ok(script_sel) = Selector::parse("script, style") else {
        return html.to_owned();
    };

    // Collect the IDs of nodes to remove.
    let to_remove: Vec<_> = fragment.select(&script_sel).map(|el| el.id()).collect();

    if to_remove.is_empty() {
        // Nothing to strip — return cheaply.
        return html.to_owned();
    }

    // scraper's Dom is read-only, so we rebuild by serialising the fragment
    // and doing a second parse with the offending nodes removed via a negative
    // CSS selector approach: select everything that is NOT script/style and
    // reconstruct the outer HTML.  The simplest correct approach is to use
    // html5ever's serialiser directly on the fragment tree, skipping the
    // unwanted nodes.
    //
    // Since scraper doesn't expose mutable tree editing, we use a regex-free
    // string reconstruction: serialise each top-level child that is not a
    // script/style element, recursively.  For deep trees we rely on the fact
    // that inner_html() on a non-script/style element already omits its own
    // tag — so we collect outer_html() of every child that survives the filter.
    let root = fragment.root_element();
    let mut out = String::with_capacity(html.len());
    for child in root.children() {
        if let Some(el) = scraper::ElementRef::wrap(child) {
            let tag = el.value().name();
            if tag == "script" || tag == "style" {
                continue;
            }
            out.push_str(&el.html());
        } else if let Some(text) = child.value().as_text() {
            // Text node — include as-is.
            out.push_str(text);
        }
    }
    out
}

/// Last-resort fallback when `htmd::convert` fails: extract plain text nodes
/// from an HTML fragment via `scraper`, which — unlike `htmd::convert` — has
/// no failure mode on malformed input. Used only so a response already
/// committed to `Content-Type: text/markdown` never carries literal HTML
/// markup in its body.
fn plain_text_fallback(html: &str) -> String {
    let fragment = scraper::Html::parse_fragment(html);
    fragment
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Inspect the upstream response headers and decide whether Markdown conversion should
/// proceed.  Cancels (`ctx.wants_markdown = false`) for anything other than a successful
/// (2xx) `text/html` response, or when the connection is SSE/WebSocket.
///
/// Also adds `Vary: Accept` when conversion is confirmed so downstream caches key
/// correctly on the `Accept` header.
///
/// Extracted as a free function so it can be unit-tested without a live Pingora session.
pub(super) fn apply_markdown_upstream_gate(
    upstream_response: &mut ResponseHeader,
    ctx: &mut ProxyContext,
) {
    if !ctx.wants_markdown {
        return;
    }

    let status = upstream_response.status.as_u16();

    // Use lowercase for case-insensitive comparison — some upstreams send "TEXT/HTML".
    let upstream_ct = upstream_response
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let is_success = (200..300).contains(&status);
    let is_html = upstream_ct.contains("text/html");
    let has_ct = !upstream_ct.is_empty();

    // The converter reads the body as UTF-8 HTML. We ask the upstream for an
    // identity body (see `request_identity_encoding_for_markdown`), but an
    // upstream is free to compress anyway; converting those bytes produces
    // mojibake under a text/markdown header. Pass such responses through
    // untouched instead — the client gets valid (compressed) HTML.
    // Every Content-Encoding field and every coding in each: a response can
    // carry `identity` in one field and `gzip` in another, or `gzip, br` in one.
    // Checked in place, no allocation: this runs for every Markdown response.
    // A header value that isn't visible ASCII can't be verified as identity,
    // so it counts as encoded.
    let is_encoded = upstream_response
        .headers
        .get_all("content-encoding")
        .iter()
        .any(|value| match value.to_str() {
            Ok(codings) => codings
                .split(',')
                .map(str::trim)
                .any(|coding| !coding.is_empty() && !coding.eq_ignore_ascii_case("identity")),
            Err(_) => true,
        });

    // Reject bodies we already know are too large from Content-Length, before
    // we commit to a text/markdown Content-Type in response_filter. Pingora
    // sends response headers to the client before response_body_filter runs,
    // so once we say "markdown" we cannot take it back — the only safe time
    // to opt out over size is here, before headers are sent. Chunked/unknown-
    // length upstreams are still capped in response_body_filter_inner.
    let declared_too_large = upstream_response
        .headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|len| len > MAX_MARKDOWN_BODY_BYTES);

    if ctx.is_sse || ctx.is_websocket || !is_success || !is_html || declared_too_large || is_encoded
    {
        // Cannot or should not convert — reset the flag so response_body_filter
        // will pass the body through normally.
        ctx.wants_markdown = false;
        if !has_ct {
            debug!(
                "Markdown conversion cancelled: no Content-Type header (status={})",
                status
            );
        } else if !is_success {
            debug!(
                "Markdown conversion cancelled: non-2xx status={}, content-type={:?}",
                status, upstream_ct
            );
        } else if is_encoded {
            debug!(
                "Markdown conversion cancelled: upstream sent Content-Encoding {:?} \
                 (content-type={:?})",
                upstream_response
                    .headers
                    .get_all("content-encoding")
                    .iter()
                    .collect::<Vec<_>>(),
                upstream_ct
            );
        } else if declared_too_large {
            debug!(
                "Markdown conversion cancelled: Content-Length exceeds {}-byte limit \
                 (content-type={:?})",
                MAX_MARKDOWN_BODY_BYTES, upstream_ct
            );
        } else {
            debug!(
                "Markdown conversion cancelled: content-type={:?}, sse={}, ws={}",
                upstream_ct, ctx.is_sse, ctx.is_websocket
            );
        }
    } else {
        // Inform downstream caches that the response varies by Accept header.
        if let Err(e) = upstream_response.insert_header("Vary", "Accept") {
            warn!("Failed to insert Vary header for markdown response: {}", e);
        }
        debug!(
            "Markdown conversion confirmed: status={}, content-type={:?}",
            status, upstream_ct
        );
    }
}

/// Ask the upstream for an uncompressed body when the client wants Markdown.
///
/// `upstream_compression.adjust_level(0)` only stops Pingora from compressing
/// the response itself; the client's own `Accept-Encoding` (browsers and most
/// HTTP clients send `gzip, br`) is still forwarded, so a compressing upstream
/// (Next.js, nginx) answers with a gzip body the HTML-to-Markdown converter
/// cannot read.
pub(super) fn request_identity_encoding_for_markdown(upstream_request: &mut RequestHeader) {
    if let Err(e) = upstream_request.insert_header("Accept-Encoding", "identity") {
        warn!("Failed to set Accept-Encoding for markdown request: {}", e);
    }
}

/// Compression level for responses Pingora compresses on the client's behalf.
pub(super) const RESPONSE_COMPRESSION_LEVEL: u32 = 6;

/// Re-enable response compression for a Markdown request whose response is
/// being passed through unconverted (JSON, an error, oversized or already
/// encoded HTML).
///
/// For Markdown requests compression is turned off and the upstream is asked
/// for `identity`, so without this a large pass-through response would reach
/// a client that accepts gzip uncompressed. Pingora records the accepted
/// encodings from the *upstream* request, which by then says `identity`, so
/// the client's original header (saved before the rewrite) is fed back in.
/// HTTP version the proxy must answer an HTTP/1.x client with.
///
/// An intermediary sends its own HTTP version on each hop (RFC 9110 §6.2), so
/// an HTTP/1.1 client gets `HTTP/1.1` even when the backend answered
/// `HTTP/1.0` (Python's `http.server`, many embedded and devtools servers).
/// Forwarding the backend's `1.0` is not just cosmetic: when the proxy gzips
/// the body, Pingora switches it to `Transfer-Encoding: chunked`, which
/// HTTP/1.0 does not define. Browsers then read the chunk framing as body bytes
/// and fail with `ERR_CONTENT_DECODING_FAILED`.
///
/// Returns `None` when the upstream version can be forwarded unchanged
/// (HTTP/2 downstreams, where the header version is not written to the wire,
/// and HTTP/1.0 clients, whose responses are never compressed).
pub(super) fn downstream_response_version(
    client: pingora_http::Version,
    upstream: pingora_http::Version,
) -> Option<pingora_http::Version> {
    (client == pingora_http::Version::HTTP_11
        && matches!(
            upstream,
            pingora_http::Version::HTTP_09 | pingora_http::Version::HTTP_10
        ))
    .then_some(pingora_http::Version::HTTP_11)
}

pub(super) fn restore_client_compression(
    compression: &mut ResponseCompressionCtx,
    accept_encoding: &str,
) {
    compression.adjust_level(RESPONSE_COMPRESSION_LEVEL);
    let mut req = match RequestHeader::build("GET", b"/", None) {
        Ok(req) => req,
        Err(e) => {
            warn!("Failed to build header for restoring compression: {}", e);
            return;
        }
    };
    if let Err(e) = req.insert_header("Accept-Encoding", accept_encoding) {
        warn!("Failed to restore Accept-Encoding for compression: {}", e);
        return;
    }
    compression.request_filter(&req);
}

/// Rewrite outbound response headers for Markdown delivery.
/// Must be called from `response_filter` (before the body is sent to the client).
///
/// Extracted as a free function so it can be unit-tested without a live Pingora session.
pub(super) fn apply_markdown_response_headers(
    upstream_response: &mut ResponseHeader,
    ctx: &ProxyContext,
) {
    if !ctx.wants_markdown {
        return;
    }
    if let Err(e) = upstream_response.insert_header("Content-Type", "text/markdown; charset=utf-8")
    {
        warn!("Failed to set Content-Type for markdown response: {}", e);
    }
    // Remove Content-Length — the Markdown body will differ in size from the HTML.
    // Pingora will handle framing via chunked transfer encoding.
    upstream_response.remove_header("Content-Length");
    // The gate only lets identity-encoded bodies through, so this is at most
    // `Content-Encoding: identity`; drop it since the body is rewritten.
    upstream_response.remove_header("Content-Encoding");
    // Set x-markdown-tokens to 0 as a placeholder.  The actual token count is
    // computed in response_body_filter once the full body is available, but
    // Pingora sends headers before the body filter runs.
    if let Err(e) = upstream_response.insert_header("X-Markdown-Tokens", "0") {
        warn!("Failed to set X-Markdown-Tokens header: {}", e);
    }
}

/// Core response-body-filter logic (SSE/WebSocket passthrough, buffered
/// Markdown conversion, default passthrough). Split out as a free function
/// so `response_body_filter` can wrap it with byte-counting that applies
/// uniformly to every exit path — see `ProxyContext::upstream_body_bytes_received`.
pub(super) fn response_body_filter_inner(
    body: &mut Option<Bytes>,
    end_of_stream: bool,
    ctx: &mut ProxyContext,
) -> Result<Option<std::time::Duration>> {
    // For SSE or WebSocket responses, pass through immediately without buffering
    if ctx.is_sse || ctx.is_websocket {
        if let Some(chunk) = body {
            let stream_type = if ctx.is_sse { "SSE" } else { "WebSocket" };
            debug!("Streaming {} chunk: {} bytes", stream_type, chunk.len());
        }
        return Ok(None);
    }

    // HTML-to-Markdown conversion: buffer chunks, convert on end_of_stream.
    if ctx.wants_markdown {
        if let Some(chunk) = body.take() {
            // Enforce a 2 MB cap — mirrors Cloudflare's Markdown for Agents constraint.
            //
            // We must NOT fall back to raw-HTML passthrough here even though the
            // buffer is over budget: response_filter already sent the client a
            // `Content-Type: text/markdown` header before this function ever runs
            // (Pingora sends response headers before invoking response_body_filter),
            // so there is no way to un-promise Markdown at this point. Emitting the
            // untouched HTML bytes under that header is exactly the "text/markdown
            // returns raw HTML" bug — instead we truncate the buffer at the cap and
            // still convert what we have, discarding the remainder of the upstream
            // body rather than forwarding it unconverted.
            let remaining = MAX_MARKDOWN_BODY_BYTES.saturating_sub(ctx.markdown_buffer.len());
            if remaining > 0 {
                let take = remaining.min(chunk.len());
                ctx.markdown_buffer.extend_from_slice(&chunk[..take]);
                if take < chunk.len() {
                    warn!(
                        "Response body for path={} exceeds the {}-byte markdown conversion \
                         limit; truncating before conversion",
                        ctx.path, MAX_MARKDOWN_BODY_BYTES
                    );
                }
            }
        }

        if end_of_stream {
            let html = String::from_utf8_lossy(&ctx.markdown_buffer);
            // Parse the document once — reuse it for both meta extraction
            // and content extraction.
            let document = scraper::Html::parse_document(&html);
            let meta = extract_page_meta(&document);
            // Extract <main> (or <body> fallback), stripping script/style.
            let content = extract_content_html(&document);
            let markdown = match htmd::convert(&content) {
                Ok(md) => md,
                Err(e) => {
                    // Cannot fall back to the original HTML bytes here: response_filter
                    // already committed `Content-Type: text/markdown` to the client
                    // before this body was available (see the truncation comment
                    // above), so raw HTML would arrive mislabeled as Markdown. Use a
                    // tag-stripping plain-text extraction instead — it cannot fail —
                    // so the body is always actual text under a text/markdown header.
                    warn!(
                        "HTML-to-Markdown conversion failed for path={}: {}; falling back to \
                         plain-text extraction",
                        ctx.path, e
                    );
                    plain_text_fallback(&content)
                }
            };

            let token_estimate = estimate_markdown_tokens(&markdown);
            debug!(
                "Markdown conversion complete for path={}: {} bytes, ~{} tokens",
                ctx.path,
                markdown.len(),
                token_estimate
            );

            // The x-markdown-tokens header must be a trailer because the response
            // headers have already been sent. Pingora does not support HTTP trailers
            // for regular HTTP/1.1 clients, so we log the value and skip injecting it
            // into headers here — the header is set in response_filter instead via
            // a sentinel value once we know the body size upfront (not possible when
            // streaming).  Best-effort: we set it here anyway; Pingora will silently
            // drop it if trailers are unsupported.
            // Note: if you need reliable x-markdown-tokens delivery, switch to a
            // buffered response pattern (write_response_* directly in request_filter).

            // Prepend YAML front-matter built from <head> meta tags,
            // matching Cloudflare's Markdown for Agents output format.
            let final_markdown = match meta.to_frontmatter() {
                Some(fm) => fm + &markdown,
                None => markdown,
            };

            ctx.markdown_buffer = Vec::new(); // free memory
            *body = Some(Bytes::from(final_markdown));
        }
        // Suppress intermediate chunks — only emit on end_of_stream.
        return Ok(None);
    }

    // Default: pass all responses through without buffering
    Ok(None)
}

#[cfg(test)]
pub(super) mod tests;
