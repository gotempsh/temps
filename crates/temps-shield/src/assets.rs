// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The challenge page and the solver it loads (ADR 066, I1), so every
//! ingress serves them itself and a worker never needs the control plane to
//! clear a visitor.

/// Where the challenge page posts its solution. Answered by the ingress on
/// every project host; it never reaches the console or the application.
pub const NEUTRAL_VERIFY_PATH: &str = "/.well-known/security-check/verify";
pub const NEUTRAL_SOLVER_JS_PATH: &str = "/.well-known/security-check/solver.js";
pub const NEUTRAL_SOLVER_WASM_PATH: &str = "/.well-known/security-check/solver_bg.wasm";
pub static NEUTRAL_SOLVER_JS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    SOLVER_JS.replace(
        "new URL('temps_captcha_wasm_bg.wasm'",
        "new URL('solver_bg.wasm'",
    )
});

pub const VERIFY_PATH: &str = "/api/_temps/captcha/verify";

/// The solver's JavaScript glue and its WebAssembly, as the page imports
/// them. Never challenged.
pub const SOLVER_JS_PATH: &str = "/api/__temps/temps_captcha_wasm.js";
pub const SOLVER_WASM_PATH: &str = "/api/__temps/temps_captcha_wasm_bg.wasm";

pub const SOLVER_JS: &str = include_str!("../../temps-captcha-wasm/pkg/temps_captcha_wasm.js");
pub const SOLVER_WASM: &[u8] =
    include_bytes!("../../temps-captcha-wasm/pkg/temps_captcha_wasm_bg.wasm");

/// The verify body is a token, a nonce and a number; anything larger is not
/// a solution.
pub const MAX_VERIFY_BODY: usize = 4 * 1024;

const CHALLENGE_HTML: &str = include_str!("../assets/challenge.html");

/// Whether `path` is one of the paths Shield answers itself and never
/// challenges: the verify endpoint and the solver.
pub fn is_shield_path(path: &str) -> bool {
    path == VERIFY_PATH
        || path == SOLVER_JS_PATH
        || path == SOLVER_WASM_PATH
        || path == NEUTRAL_VERIFY_PATH
        || path == NEUTRAL_SOLVER_JS_PATH
        || path == NEUTRAL_SOLVER_WASM_PATH
}

/// The challenge page for `token`. Every inserted value is escaped, though
/// the token is base64url and the address a parsed IP.
pub fn challenge_page(token: &str, difficulty: u8, client_ip: &str) -> String {
    CHALLENGE_HTML
        .replace("{{TOKEN}}", &escape_html(token))
        .replace("{{DIFFICULTY}}", &difficulty.to_string())
        .replace("{{IP_ADDRESS}}", &escape_html(client_ip))
}

/// Preserve the challenge protocol while omitting its visible product title.
pub fn challenge_page_with_identity(
    token: &str,
    difficulty: u8,
    client_ip: &str,
    hide: bool,
) -> String {
    let page = challenge_page(token, difficulty, client_ip);
    if hide {
        page.replace("Temps Security Checkpoint", "Security checkpoint")
            .replace(
                "<!-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors -->",
                "",
            )
            .replace(SOLVER_JS_PATH, NEUTRAL_SOLVER_JS_PATH)
            .replace(VERIFY_PATH, NEUTRAL_VERIFY_PATH)
    } else {
        page
    }
}

pub fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    #[test]
    fn hidden_identity_keeps_challenge_protocol_and_omits_branding() {
        let page = super::challenge_page_with_identity("token", 20, "127.0.0.1", true);
        assert!(!page.to_lowercase().contains("temps"));
        for path in [
            super::NEUTRAL_VERIFY_PATH,
            super::NEUTRAL_SOLVER_JS_PATH,
            super::NEUTRAL_SOLVER_WASM_PATH,
        ] {
            assert!(super::is_shield_path(path));
        }
        assert!(page.contains(super::NEUTRAL_VERIFY_PATH));
        assert!(page.contains(super::NEUTRAL_SOLVER_JS_PATH));
        assert!(super::NEUTRAL_SOLVER_JS.contains("new URL('solver_bg.wasm'"));
        assert_eq!(
            super::challenge_page_with_identity("token", 20, "127.0.0.1", false),
            super::challenge_page("token", 20, "127.0.0.1")
        );
    }

    use super::*;

    #[test]
    fn page_escapes_what_it_inserts() {
        let page = challenge_page("tok", 20, "<script>alert(1)</script>");
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!page.contains("<script>alert(1)"));
        assert!(page.contains(r#"data-token="tok""#));
        assert!(page.contains(r#"data-difficulty="20""#));
        assert!(!page.contains("{{"), "every placeholder is filled");
    }

    #[test]
    fn page_posts_only_token_nonce_and_timing() {
        // The old body named the environment and identifier; the ingress
        // now derives both, and the page must not send them.
        assert!(!CHALLENGE_HTML.contains("environment_id"));
        assert!(!CHALLENGE_HTML.contains("identifier"));
        assert!(CHALLENGE_HTML.contains(VERIFY_PATH));
        assert!(CHALLENGE_HTML.contains(SOLVER_JS_PATH));
    }

    #[test]
    fn the_solver_ships_with_the_page() {
        assert!(SOLVER_WASM.starts_with(b"\0asm"));
        assert!(SOLVER_JS.contains("solve_challenge"));
        assert!(is_shield_path(SOLVER_WASM_PATH) && !is_shield_path("/"));
    }
}
