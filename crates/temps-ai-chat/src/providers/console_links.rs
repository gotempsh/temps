// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The console pages the assistant may link to, so a reply that mentions a
//! project, a deployment or a backup can point at it.
//!
//! The table is the assistant's whole knowledge of the console's routes. A
//! test reads the web router and fails if a path here is not a real route, so
//! a page that is renamed or removed cannot leave the assistant linking to a
//! "not found" screen.

/// One console page and what it shows.
struct ConsoleLink {
    what: &'static str,
    /// The route as the web router declares it; `:name` is a value to fill in.
    path: &'static str,
}

const fn link(what: &'static str, path: &'static str) -> ConsoleLink {
    ConsoleLink { what, path }
}

/// Kept short on purpose: every entry is sent with every request, so only
/// the entities a reply most often names are here.
const CONSOLE_LINKS: &[ConsoleLink] = &[
    link("project", "/projects/:slug"),
    link("deployment", "/projects/:slug/deployments/:deploymentId"),
    link("error group", "/projects/:slug/errors/:errorGroupId"),
    link("database or service", "/storage/:id"),
    link("domain", "/domains/:id"),
    link("S3 source", "/backups/s3-sources/:id"),
    link("backup schedule", "/backups/schedules/:id"),
    link("backup run", "/backups/schedules/:scheduleId/runs/:runId"),
    link("sandbox", "/sandboxes/:sandboxId"),
    link("Git provider", "/git-providers/:id"),
];

/// The prompt section that tells the assistant which pages exist and how to
/// link them.
pub fn console_links_guidance() -> String {
    let mut section = String::from(
        "## Console links\n\
         Link an entity where you first name it, as a relative markdown link (it opens in a new tab), e.g. `[nightly backups](/backups/schedules/2)`. \
         `:slug` is the project's slug, other placeholders are ids. Use only values from tool results; never guess one, and without it do not link.\n",
    );
    for entry in CONSOLE_LINKS {
        section.push_str(&format!("- {}: `{}`\n", entry.what, entry.path));
    }
    section
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The router's top-level routes and the project page's own routes.
    const APP_ROUTES: &str = include_str!("../../../../web/src/App.tsx");
    const PROJECT_ROUTES: &str = include_str!("../../../../web/src/pages/ProjectDetail.tsx");

    /// Whether the router declares `route`, exactly or as a prefix that owns
    /// everything beneath it (`logs/*`).
    fn declared(source: &str, route: &str) -> bool {
        source.contains(&format!("path=\"{route}\""))
            || source.contains(&format!("path=\"{route}/*\""))
    }

    fn is_a_real_route(path: &str) -> bool {
        if path == "/projects/:slug" {
            return declared(APP_ROUTES, "/projects/:slug");
        }
        if let Some(rest) = path.strip_prefix("/projects/:slug/") {
            return declared(PROJECT_ROUTES, rest);
        }
        declared(APP_ROUTES, path)
    }

    #[test]
    fn every_linked_path_is_a_route_the_console_serves() {
        let missing: Vec<&str> = CONSOLE_LINKS
            .iter()
            .map(|entry| entry.path)
            .filter(|path| !is_a_real_route(path))
            .collect();
        assert!(
            missing.is_empty(),
            "the assistant would link to pages the console does not have: {missing:?}. \
             Update CONSOLE_LINKS in console_links.rs to the router's current paths."
        );
    }

    #[test]
    fn a_path_is_listed_once_and_is_site_relative() {
        let mut seen = std::collections::HashSet::new();
        for entry in CONSOLE_LINKS {
            assert!(
                entry.path.starts_with('/') && !entry.path.contains("//"),
                "'{}' must be a site-relative path",
                entry.path
            );
            assert!(seen.insert(entry.path), "'{}' is listed twice", entry.path);
        }
    }

    #[test]
    fn the_guidance_lists_every_path_and_says_not_to_guess_ids() {
        let guidance = console_links_guidance();
        for entry in CONSOLE_LINKS {
            assert!(guidance.contains(entry.path), "{} is missing", entry.path);
        }
        assert!(guidance.contains("never guess one"));
        assert!(guidance.contains("new tab"));
    }
}
