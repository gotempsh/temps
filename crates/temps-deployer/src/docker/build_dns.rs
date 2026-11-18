// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Spotting a build that failed because its container could not resolve host
//! names. Package managers bury this: npm can end with an unrelated message
//! and leave `EAI_AGAIN` in its debug log, pip and apt retry and then report a
//! generic failure. When a failed build's output shows a resolver failure, the
//! build log gets one plain line saying so, with what to check on the host.

/// Substrings resolvers and the tools on top of them print when a name could
/// not be looked up because no resolver answered. Deliberately not
/// `ENOTFOUND`/`NXDOMAIN`: those mean the resolver answered that the name does
/// not exist, which is usually a typo, not a broken resolver.
const RESOLVER_FAILURE_MARKERS: [&str; 6] = [
    // Node.js (npm, yarn, pnpm) and C's getaddrinfo constant.
    "EAI_AGAIN",
    // glibc's text for EAI_AGAIN (pip, Python, Ruby, most C programs).
    "Temporary failure in name resolution",
    // apt.
    "Temporary failure resolving",
    // musl's text for EAI_AGAIN (Alpine images).
    "Name could not be resolved at this time",
    // curl and git.
    "Could not resolve host",
    // Bundler and RubyGems.
    "getaddrinfo: Try again",
];

/// Whether build output shows a host-name lookup failing for want of a
/// resolver.
fn output_reports_resolver_failure(output: &str) -> bool {
    if RESOLVER_FAILURE_MARKERS
        .iter()
        .any(|marker| output.contains(marker))
    {
        return true;
    }
    // Go names the resolver it failed to reach: `lookup proxy.golang.org on
    // 169.254.169.254:53: ... connection refused` (or `i/o timeout`).
    output.contains("lookup ") && output.contains(":53: ")
}

/// Watches a build's output, chunk by chunk, for a resolver failure. Stops
/// looking once it has seen one, so a long build pays nothing further.
#[derive(Debug, Default)]
pub(super) struct BuildDnsWatch {
    resolver_failed: bool,
}

impl BuildDnsWatch {
    pub(super) fn observe(&mut self, output: &str) {
        if !self.resolver_failed {
            self.resolver_failed = output_reports_resolver_failure(output);
        }
    }

    /// The build-log line for a failed build whose output (or final error,
    /// `error_text`) shows a resolver failure; `None` otherwise.
    pub(super) fn failure_hint(&self, error_text: &str) -> Option<String> {
        if !self.resolver_failed && !output_reports_resolver_failure(error_text) {
            return None;
        }
        Some(
            "ERROR: DNS resolution failed inside the build container: a build step could not \
             look up a host name because no DNS resolver answered (EAI_AGAIN / temporary failure \
             in name resolution). The failure above is most likely a consequence of this, not a \
             problem in your code.\n\
             HINT: on the build host, run `docker run --rm alpine nslookup github.com`. If it \
             fails, check the nameservers Docker gives containers (/etc/resolv.conf, or \
             /run/systemd/resolve/resolv.conf with systemd-resolved, or \"dns\" in \
             /etc/docker/daemon.json) and any host firewall rule that blocks UDP/TCP port 53 to \
             them.\n"
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_resolver_failures_from_common_build_tools() {
        for output in [
            "npm error request to https://registry.npmjs.org/left-pad failed, reason: \
             getaddrinfo EAI_AGAIN registry.npmjs.org",
            "npm error code EAI_AGAIN",
            "WARNING: Retrying ... NewConnectionError: Failed to establish a new connection: \
             [Errno -3] Temporary failure in name resolution",
            "W: Failed to fetch http://deb.debian.org/debian/dists/bookworm/InRelease  \
             Temporary failure resolving 'deb.debian.org'",
            "fetch https://dl-cdn.alpinelinux.org/alpine/v3.20/main: Name could not be resolved \
             at this time",
            "curl: (6) Could not resolve host: example.com",
            "fatal: unable to access 'https://example.com/repo.git/': Could not resolve host: \
             example.com",
            "go: example.com/mod@v1.0.0: Get \"https://proxy.golang.org/...\": dial tcp: lookup \
             proxy.golang.org on 169.254.169.254:53: read udp 172.17.0.2:41000->169.254.169.254:53: \
             read: connection refused",
            "Could not fetch specs from https://rubygems.org/ due to underlying error \
             <Socket::ResolutionError: getaddrinfo: Try again>",
        ] {
            assert!(
                output_reports_resolver_failure(output),
                "not recognised as a resolver failure: {output}"
            );
        }
    }

    #[test]
    fn ignores_names_that_do_not_exist_and_unrelated_failures() {
        for output in [
            // The resolver answered: the name is wrong, not the resolver.
            "npm error getaddrinfo ENOTFOUND registry.npmjs.invalid",
            "npm error Exit handler never called!",
            "error: failed to compile `app`",
            "lookup table built in 53 ms",
            "process \"/bin/sh -c npm ci\" did not complete successfully: exit code: 1",
        ] {
            assert!(
                !output_reports_resolver_failure(output),
                "wrongly recognised as a resolver failure: {output}"
            );
        }
    }

    #[test]
    fn hint_only_after_a_resolver_failure_was_seen() {
        let mut watch = BuildDnsWatch::default();
        watch.observe("#5 [deps 2/3] RUN npm ci\n");
        assert_eq!(watch.failure_hint("exit code: 1"), None);

        watch.observe("npm error code EAI_AGAIN\n");
        // Later output does not reset what was seen.
        watch.observe("npm error Exit handler never called!\n");
        let hint = watch
            .failure_hint("exit code: 1")
            .expect("hint after EAI_AGAIN");
        assert!(hint.starts_with("ERROR: DNS resolution failed inside the build container"));
        assert!(hint.contains("docker run --rm alpine nslookup github.com"));
        assert!(hint.contains("port 53"));
        assert!(hint.ends_with('\n'));
    }

    #[test]
    fn hint_from_the_final_error_alone() {
        let watch = BuildDnsWatch::default();
        assert!(watch
            .failure_hint(
                "failed to solve: failed to fetch remote https://example.com/archive.tgz: \
                 Temporary failure in name resolution"
            )
            .is_some());
    }
}
