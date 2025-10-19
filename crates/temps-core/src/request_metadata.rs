use axum::http::HeaderMap;

#[derive(Clone)]
pub struct RequestMetadata {
    pub ip_address: String,
    pub user_agent: String,
    pub headers: HeaderMap,
    pub visitor_id_cookie: Option<String>,
    pub session_id_cookie: Option<String>,
    pub base_url: String,
    pub scheme: String,  // "http" or "https"
    pub host: String,    // hostname from Host header
    pub is_secure: bool, // true if HTTPS
}
