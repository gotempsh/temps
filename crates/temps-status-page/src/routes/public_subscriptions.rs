// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Public subscribe, confirm and unsubscribe routes of status pages
//! (ADR 108 D2).
//!
//! Like the page itself, every route first finds the page in the in-memory
//! snapshot cache: an unknown, unpublished or wrong-host page gets the
//! standard 404 without a database query. Then:
//!
//! - `POST /status/{slug}/subscribe` stores a pending request (form post, at
//!   most 8 KiB). The answer is the same whether the address is new, pending
//!   or already confirmed.
//! - `GET /status/{slug}/confirm/{token}` and `.../unsubscribe/{token}` only
//!   render a page with a button, without touching the database, so a mail
//!   scanner that opens links confirms or unsubscribes nothing. The button
//!   posts to the same path, which acts.
//!
//! These routes are rate-limited per client far below the page itself.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{rejection::FormRejection, ConnectInfo, DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Form, Router,
};
use serde::Deserialize;
use temps_core::public_status_page::{NOT_FOUND_HTML, SECURITY_HEADERS, STATUS_HOST_HEADER};

use super::public::PublicRateLimiter;
use crate::services::public_pages::render::render_notice_page;
use crate::services::public_pages::{is_well_formed_slug, PublishedSnapshot, StatusSnapshotCache};
use crate::services::subscribers::tokens::is_well_formed_token;
use crate::services::subscribers::{PublicOutcome, StatusPageSubscriberService};

/// Subscription requests per client per window: enough for a person, far too
/// few to enumerate or flood.
pub const SUBSCRIPTION_PER_CLIENT_LIMIT: u32 = 10;
/// Subscription requests per window from clients that could not be tracked.
pub const SUBSCRIPTION_OVERFLOW_LIMIT: u32 = 600;
pub const SUBSCRIPTION_WINDOW: Duration = Duration::from_secs(600);
/// Largest subscribe form body.
pub const MAX_FORM_BYTES: usize = 8 * 1024;

pub struct PublicSubscriptionState {
    pub cache: Arc<StatusSnapshotCache>,
    pub subscribers: Arc<StatusPageSubscriberService>,
    pub limiter: PublicRateLimiter,
}

impl PublicSubscriptionState {
    pub fn new(
        cache: Arc<StatusSnapshotCache>,
        subscribers: Arc<StatusPageSubscriberService>,
    ) -> Self {
        Self {
            cache,
            subscribers,
            limiter: PublicRateLimiter::new(
                SUBSCRIPTION_PER_CLIENT_LIMIT,
                SUBSCRIPTION_OVERFLOW_LIMIT,
                SUBSCRIPTION_WINDOW,
            ),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SubscribeForm {
    #[serde(default)]
    pub email: String,
}

/// The custom hostname the proxy tagged this request with, normalized the
/// same way as the page routes do.
fn custom_host(headers: &HeaderMap) -> Option<String> {
    headers.get(STATUS_HOST_HEADER).map(|value| {
        value
            .to_str()
            .map(|host| host.trim().trim_end_matches('.').to_ascii_lowercase())
            .unwrap_or_default()
    })
}

/// Where the page's routes live for this request: the hostname root on a
/// custom hostname, the slug path otherwise.
fn base_path(custom_host: Option<&str>, slug: &str) -> String {
    if custom_host.is_some() {
        String::new()
    } else {
        format!("/api/status/{slug}")
    }
}

fn page_home(custom_host: Option<&str>, slug: &str) -> String {
    if custom_host.is_some() {
        "/".to_string()
    } else {
        format!("/api/status/{slug}")
    }
}

fn harden(mut response: Response) -> Response {
    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

fn html(status: StatusCode, body: String) -> Response {
    harden(
        (
            status,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            body,
        )
            .into_response(),
    )
}

fn not_found() -> Response {
    harden(
        (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            NOT_FOUND_HTML,
        )
            .into_response(),
    )
}

fn find(
    state: &PublicSubscriptionState,
    headers: &HeaderMap,
    slug: &str,
) -> Option<(Arc<PublishedSnapshot>, Option<String>)> {
    let host = custom_host(headers);
    let snapshot = is_well_formed_slug(slug)
        .then(|| state.cache.get(slug))
        .flatten()
        .filter(|snapshot| snapshot.serves_host(host.as_deref()))
        .filter(|snapshot| snapshot.accepts_subscriptions())?;
    Some((snapshot, host))
}

fn message(
    snapshot: &PublishedSnapshot,
    host: Option<&str>,
    slug: &str,
    status: StatusCode,
    heading: &str,
    body: &str,
    form: Option<(&str, &str)>,
) -> Response {
    html(
        status,
        render_notice_page(
            &snapshot.title,
            &snapshot.appearance,
            heading,
            body,
            form,
            &page_home(host, slug),
        ),
    )
}

fn outcome_page(
    snapshot: &PublishedSnapshot,
    host: Option<&str>,
    slug: &str,
    outcome: PublicOutcome,
) -> Response {
    let (status, heading, body) = match outcome {
        PublicOutcome::Accepted => (
            StatusCode::OK,
            "Check your inbox",
            "If the address can receive mail, a confirmation link is on its way. Follow it to \
             start receiving updates. Nothing is sent until you do.",
        ),
        PublicOutcome::InvalidEmail => (
            StatusCode::BAD_REQUEST,
            "That does not look like an email address",
            "Go back and enter the address you want updates sent to.",
        ),
        PublicOutcome::Full => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Subscriptions are not available right now",
            "This page cannot take new subscriptions at the moment. Try again later.",
        ),
        PublicOutcome::Confirmed => (
            StatusCode::OK,
            "You are subscribed",
            "You will get an email when an incident on this page is posted or updated. Every \
             email has a link to unsubscribe.",
        ),
        PublicOutcome::LinkInvalid => (
            StatusCode::NOT_FOUND,
            "This link has expired or was already used",
            "Subscribe again from the status page to get a new confirmation link.",
        ),
        PublicOutcome::Unsubscribed => (
            StatusCode::OK,
            "You are unsubscribed",
            "This address will not get any more updates from this page, and it has been \
             removed from the subscriber list.",
        ),
    };
    message(snapshot, host, slug, status, heading, body, None)
}

fn service_error(
    error: crate::services::subscribers::SubscriptionError,
    slug_page: i32,
) -> Response {
    tracing::error!(
        status_page_id = slug_page,
        error = %error,
        "public status page subscription request failed"
    );
    html(
        StatusCode::SERVICE_UNAVAILABLE,
        render_notice_page(
            "Status page",
            &Default::default(),
            "Something went wrong",
            "Your request could not be processed. Try again in a few minutes.",
            None,
            "/",
        ),
    )
}

async fn rate_limit(
    State(state): State<Arc<PublicSubscriptionState>>,
    request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let client = temps_core::resolve_client_ip(request.headers(), peer);
    match state.limiter.check(&client) {
        Ok(()) => next.run(request).await,
        Err(limited) => {
            if let Some(total) = limited.log_total {
                tracing::warn!(
                    rejected_total = total,
                    retry_after_secs = limited.retry_after_secs,
                    "public status page subscription requests are being rate-limited (logged once per window)"
                );
            }
            let mut response = harden(
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    "Too many requests",
                )
                    .into_response(),
            );
            if let Ok(value) = HeaderValue::from_str(&limited.retry_after_secs.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
            response
        }
    }
}

/// Subscribe to a status page by email
#[utoipa::path(
    post,
    path = "/status/{slug}/subscribe",
    operation_id = "status_page_subscribe",
    params(("slug" = String, Path, description = "Unguessable status page token")),
    request_body(content_type = "application/x-www-form-urlencoded", description = "`email=<address>`"),
    responses(
        (status = 200, description = "Request accepted; a confirmation link is sent", content_type = "text/html"),
        (status = 400, description = "Not an email address", content_type = "text/html"),
        (status = 404, description = "No published status page taking subscriptions at this URL"),
        (status = 429, description = "Rate limited"),
        (status = 503, description = "The page cannot take more subscriptions", content_type = "text/html"),
    ),
    tag = "Status Page"
)]
pub async fn subscribe(
    State(state): State<Arc<PublicSubscriptionState>>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    form: Result<Form<SubscribeForm>, FormRejection>,
) -> Response {
    let Some((snapshot, host)) = find(&state, &headers, &slug) else {
        return not_found();
    };
    let email = form.map(|Form(form)| form.email).unwrap_or_default();
    match state.subscribers.subscribe(&snapshot, &email).await {
        Ok(outcome) => outcome_page(&snapshot, host.as_deref(), &slug, outcome),
        Err(error) => service_error(error, snapshot.status_page_id),
    }
}

/// Confirmation link: asks for a click
#[utoipa::path(
    get,
    path = "/status/{slug}/confirm/{token}",
    operation_id = "status_page_confirm_form",
    params(
        ("slug" = String, Path, description = "Unguessable status page token"),
        ("token" = String, Path, description = "Confirmation token from the mail"),
    ),
    responses(
        (status = 200, description = "Page with a confirm button", content_type = "text/html"),
        (status = 404, description = "Unknown page or malformed link"),
    ),
    tag = "Status Page"
)]
pub async fn confirm_form(
    State(state): State<Arc<PublicSubscriptionState>>,
    headers: HeaderMap,
    Path((slug, token)): Path<(String, String)>,
) -> Response {
    let Some((snapshot, host)) = find(&state, &headers, &slug) else {
        return not_found();
    };
    if !is_well_formed_token(&token) {
        return outcome_page(
            &snapshot,
            host.as_deref(),
            &slug,
            PublicOutcome::LinkInvalid,
        );
    }
    let action = format!("{}/confirm/{token}", base_path(host.as_deref(), &slug));
    message(
        &snapshot,
        host.as_deref(),
        &slug,
        StatusCode::OK,
        "Confirm your subscription",
        "Confirm to receive an email when an incident on this page is posted or updated.",
        Some((&action, "Confirm subscription")),
    )
}

/// Confirm a subscription
#[utoipa::path(
    post,
    path = "/status/{slug}/confirm/{token}",
    operation_id = "status_page_confirm",
    params(
        ("slug" = String, Path, description = "Unguessable status page token"),
        ("token" = String, Path, description = "Confirmation token from the mail"),
    ),
    responses(
        (status = 200, description = "Subscription confirmed", content_type = "text/html"),
        (status = 404, description = "Unknown page, or the link expired or was used"),
        (status = 429, description = "Rate limited"),
    ),
    tag = "Status Page"
)]
pub async fn confirm(
    State(state): State<Arc<PublicSubscriptionState>>,
    headers: HeaderMap,
    Path((slug, token)): Path<(String, String)>,
) -> Response {
    let Some((snapshot, host)) = find(&state, &headers, &slug) else {
        return not_found();
    };
    if !is_well_formed_token(&token) {
        return outcome_page(
            &snapshot,
            host.as_deref(),
            &slug,
            PublicOutcome::LinkInvalid,
        );
    }
    match state.subscribers.confirm(&snapshot, &token).await {
        Ok(outcome) => outcome_page(&snapshot, host.as_deref(), &slug, outcome),
        Err(error) => service_error(error, snapshot.status_page_id),
    }
}

/// Unsubscribe link: asks for a click
#[utoipa::path(
    get,
    path = "/status/{slug}/unsubscribe/{token}",
    operation_id = "status_page_unsubscribe_form",
    params(
        ("slug" = String, Path, description = "Unguessable status page token"),
        ("token" = String, Path, description = "Unsubscribe token from a mail"),
    ),
    responses(
        (status = 200, description = "Page with an unsubscribe button", content_type = "text/html"),
        (status = 404, description = "Unknown page or malformed link"),
    ),
    tag = "Status Page"
)]
pub async fn unsubscribe_form(
    State(state): State<Arc<PublicSubscriptionState>>,
    headers: HeaderMap,
    Path((slug, token)): Path<(String, String)>,
) -> Response {
    let Some((snapshot, host)) = find(&state, &headers, &slug) else {
        return not_found();
    };
    if !is_well_formed_token(&token) {
        return outcome_page(
            &snapshot,
            host.as_deref(),
            &slug,
            PublicOutcome::LinkInvalid,
        );
    }
    let action = format!("{}/unsubscribe/{token}", base_path(host.as_deref(), &slug));
    message(
        &snapshot,
        host.as_deref(),
        &slug,
        StatusCode::OK,
        "Unsubscribe",
        "Stop receiving incident updates from this page. Your address is removed from the \
         subscriber list.",
        Some((&action, "Unsubscribe")),
    )
}

/// Unsubscribe
#[utoipa::path(
    post,
    path = "/status/{slug}/unsubscribe/{token}",
    operation_id = "status_page_unsubscribe",
    params(
        ("slug" = String, Path, description = "Unguessable status page token"),
        ("token" = String, Path, description = "Unsubscribe token from a mail"),
    ),
    responses(
        (status = 200, description = "Unsubscribed (also for a link already used)", content_type = "text/html"),
        (status = 404, description = "Unknown page or malformed link"),
        (status = 429, description = "Rate limited"),
    ),
    tag = "Status Page"
)]
pub async fn unsubscribe(
    State(state): State<Arc<PublicSubscriptionState>>,
    headers: HeaderMap,
    Path((slug, token)): Path<(String, String)>,
) -> Response {
    let Some((snapshot, host)) = find(&state, &headers, &slug) else {
        return not_found();
    };
    if !is_well_formed_token(&token) {
        return outcome_page(
            &snapshot,
            host.as_deref(),
            &slug,
            PublicOutcome::LinkInvalid,
        );
    }
    match state.subscribers.unsubscribe(&snapshot, &token).await {
        Ok(outcome) => outcome_page(&snapshot, host.as_deref(), &slug, outcome),
        Err(error) => service_error(error, snapshot.status_page_id),
    }
}

/// Router for the public subscription routes, with their own rate limit.
pub fn create_public_subscription_router(state: Arc<PublicSubscriptionState>) -> Router {
    Router::new()
        .route("/status/{slug}/subscribe", post(subscribe))
        .route(
            "/status/{slug}/confirm/{token}",
            get(confirm_form).post(confirm),
        )
        .route(
            "/status/{slug}/unsubscribe/{token}",
            get(unsubscribe_form).post(unsubscribe),
        )
        .layer(DefaultBodyLimit::max(MAX_FORM_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_stay_on_the_host_the_visitor_used() {
        let slug = "0123456789abcdef0123456789abcdef";
        assert_eq!(base_path(Some("status.example.test"), slug), "");
        assert_eq!(page_home(Some("status.example.test"), slug), "/");
        assert_eq!(base_path(None, slug), format!("/api/status/{slug}"));
        assert_eq!(page_home(None, slug), format!("/api/status/{slug}"));
    }
}
