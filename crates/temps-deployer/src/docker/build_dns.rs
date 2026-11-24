// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Spotting a build that failed because its container could not resolve host
//! names. Package managers bury this: npm can end with an unrelated message
//! and leave `EAI_AGAIN` in its debug log, pip and apt retry and then report a
//! generic failure. When a failed build's output shows a resolver failure, the
//! build log gets one plain line saying so, with what to check on the host.

/// Substrings resolvers and the tools on top of them print when a name could
/// not be looked up because no resolver answered. Deliberately not
/// `ENOTFOUND`/`NXDOMAIN`/`no such host`, nor curl's `Could not resolve host`:
/// those are (or include) the resolver answering that the name does not
/// exist, which is usually a typo, not a broken resolver.
const RESOLVER_FAILURE_MARKERS: [&str; 5] = [
    // Node.js (npm, yarn, pnpm) and C's getaddrinfo constant.
    "EAI_AGAIN",
    // glibc's text for EAI_AGAIN (pip, Python, Ruby, most C programs).
    "Temporary failure in name resolution",
    // apt.
    "Temporary failure resolving",
    // musl's text for EAI_AGAIN (Alpine images).
    "Name could not be resolved at this time",
    // Bundler and RubyGems.
    "getaddrinfo: Try again",
];

/// Transport errors Go reports when the resolver it names did not answer.
const GO_RESOLVER_TRANSPORT_FAILURES: [&str; 4] = [
    "connection refused",
    "i/o timeout",
    "network is unreachable",
    "no route to host",
];

/// Longest unfinished line kept per output source to join with the next
/// chunk; far longer than any marker, short enough to never matter.
const MAX_TAIL_BYTES: usize = 512;
/// Distinct output sources (BuildKit vertex and stream) tracked at once; the
/// oldest is forgotten first.
const MAX_TAILED_SOURCES: usize = 32;

/// Whether one line of build output shows a host-name lookup failing for
/// want of a resolver.
fn line_reports_resolver_failure(line: &str) -> bool {
    if RESOLVER_FAILURE_MARKERS
        .iter()
        .any(|marker| line.contains(marker))
    {
        return true;
    }
    // Go names the resolver it failed to reach: `lookup proxy.golang.org on
    // 169.254.169.254:53: read udp ...: connection refused`. The same shape
    // ends in `no such host` when the resolver answered, so the transport
    // error is required.
    line.contains("lookup ")
        && line.contains(":53: ")
        && GO_RESOLVER_TRANSPORT_FAILURES
            .iter()
            .any(|failure| line.contains(failure))
}

fn output_reports_resolver_failure(output: &str) -> bool {
    output.lines().any(line_reports_resolver_failure)
}

/// The last `max` bytes of `text`, cut on a character boundary.
fn bounded_suffix(text: &str, max: usize) -> &str {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Watches a build's output, chunk by chunk, for a resolver failure. Output
/// can be cut anywhere (BuildKit forwards writes as they happen), so the
/// unfinished last line of each source is kept, bounded, and joined with that
/// source's next chunk; separate sources are never joined. Stops looking once
/// it has seen a failure, so a long build pays nothing further.
#[derive(Debug, Default)]
pub(super) struct BuildDnsWatch {
    resolver_failed: bool,
    /// `(vertex, stream)` → unfinished last line, oldest first.
    tails: Vec<((String, i64), String)>,
}

impl BuildDnsWatch {
    /// Output from the classic build stream, which is a single source.
    pub(super) fn observe_stream(&mut self, output: &str) {
        self.observe("", 0, output);
    }

    /// Output from one BuildKit vertex's stream (stdout or stderr).
    pub(super) fn observe(&mut self, vertex: &str, stream: i64, output: &str) {
        if self.resolver_failed {
            return;
        }
        let position = self
            .tails
            .iter()
            .position(|((v, s), _)| v == vertex && *s == stream);
        let previous = position.map(|i| self.tails.remove(i).1);
        let joined;
        let text = match previous {
            Some(mut tail) => {
                tail.push_str(output);
                joined = tail;
                joined.as_str()
            }
            None => output,
        };
        if output_reports_resolver_failure(text) {
            self.resolver_failed = true;
            self.tails = Vec::new();
            return;
        }
        let unfinished = text.rsplit_once('\n').map_or(text, |(_, rest)| rest);
        if unfinished.is_empty() {
            return;
        }
        if self.tails.len() == MAX_TAILED_SOURCES {
            self.tails.remove(0);
        }
        self.tails.push((
            (vertex.to_string(), stream),
            bounded_suffix(unfinished, MAX_TAIL_BYTES).to_string(),
        ));
    }

    /// The build-log lines for a failed build whose final error, or earlier
    /// output, shows a resolver failure; `None` otherwise. The failure is
    /// attributed to DNS only when the final error itself shows it.
    pub(super) fn failure_hint(&self, error_text: &str) -> Option<String> {
        let headline = if output_reports_resolver_failure(error_text) {
            "ERROR: DNS resolution failed inside the build container: this step failed because \
             it could not look up a host name and no DNS resolver answered (EAI_AGAIN / \
             temporary failure in name resolution).\n"
        } else if self.resolver_failed {
            "ERROR: DNS resolution failed inside the build container: earlier output of this \
             build shows a host-name lookup that no DNS resolver answered (EAI_AGAIN / temporary \
             failure in name resolution). If the failed step downloads anything (dependencies, \
             packages), this is the likely cause; otherwise start from the error above.\n"
        } else {
            return None;
        };
        Some(format!(
            "{headline}\
             HINT: check DNS from a container on the build host with an image it already has, \
             for example `docker run --rm --pull=never <the build's base image> getent hosts \
             github.com` (an image that cannot be pulled says nothing about container DNS). If the \
             lookup fails, check the nameservers Docker gives containers (/etc/resolv.conf, or \
             /run/systemd/resolve/resolv.conf with systemd-resolved, or \"dns\" in \
             /etc/docker/daemon.json) and any host firewall rule that blocks UDP/TCP port 53 to \
             them.\n"
        ))
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
            "go: example.com/mod@v1.0.0: Get \"https://proxy.golang.org/...\": dial tcp: lookup \
             proxy.golang.org on 169.254.169.254:53: read udp 172.17.0.2:41000->169.254.169.254:53: \
             read: connection refused",
            "dial tcp: lookup proxy.golang.org on 10.0.0.2:53: dial udp 10.0.0.2:53: i/o timeout",
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
            "dial tcp: lookup example.invalid on 169.254.169.254:53: no such host",
            "curl: (6) Could not resolve host: example.invalid",
            "npm error Exit handler never called!",
            "error: failed to compile `app`",
            "lookup table built in 53 ms",
            "process \"/bin/sh -c npm ci\" did not complete successfully: exit code: 1",
            // Go's shape and a transport error on different lines.
            "lookup cache on 10.0.0.2:53: warming\nupstream: connection refused",
        ] {
            assert!(
                !output_reports_resolver_failure(output),
                "wrongly recognised as a resolver failure: {output}"
            );
        }
    }

    #[test]
    fn a_marker_split_across_chunks_of_one_source_is_recognised() {
        let mut watch = BuildDnsWatch::default();
        watch.observe("sha256:aa", 2, "npm error code EAI_");
        assert!(!watch.resolver_failed);
        watch.observe("sha256:aa", 2, "AGAIN\n");
        assert!(watch.resolver_failed);

        let mut watch = BuildDnsWatch::default();
        watch.observe_stream("Temporary failure in name ");
        watch.observe_stream("resolution\n");
        assert!(watch.resolver_failed);
    }

    #[test]
    fn chunks_of_different_sources_are_never_joined() {
        let mut watch = BuildDnsWatch::default();
        watch.observe("sha256:aa", 2, "npm error code EAI_");
        watch.observe("sha256:bb", 2, "AGAIN\n");
        watch.observe("sha256:aa", 1, "AGAIN\n");
        assert!(!watch.resolver_failed);
    }

    #[test]
    fn tails_stay_bounded() {
        let mut watch = BuildDnsWatch::default();
        for vertex in 0..(MAX_TAILED_SOURCES * 2) {
            watch.observe(&format!("v{vertex}"), 1, &"é".repeat(MAX_TAIL_BYTES));
        }
        assert_eq!(watch.tails.len(), MAX_TAILED_SOURCES);
        assert!(watch
            .tails
            .iter()
            .all(|(_, tail)| tail.len() <= MAX_TAIL_BYTES));
        // A finished line leaves nothing behind.
        let mut watch = BuildDnsWatch::default();
        watch.observe_stream("Step 1/4 : FROM node\n");
        assert!(watch.tails.is_empty());
    }

    #[test]
    fn hint_after_earlier_output_does_not_blame_the_failed_step() {
        let mut watch = BuildDnsWatch::default();
        watch.observe_stream("#5 [deps 2/3] RUN npm ci\n");
        assert_eq!(watch.failure_hint("exit code: 1"), None);

        watch.observe_stream("npm error code EAI_AGAIN\n");
        // Later output does not reset what was seen.
        watch.observe_stream("npm error Exit handler never called!\n");
        let hint = watch
            .failure_hint("exit code: 1")
            .expect("hint after EAI_AGAIN");
        assert!(hint.starts_with("ERROR: DNS resolution failed inside the build container"));
        assert!(hint.contains("earlier output of this build"));
        assert!(!hint.contains("this step failed because"));
        assert!(hint.contains("--pull=never"));
        assert!(hint.contains("port 53"));
        assert!(hint.ends_with('\n'));
    }

    #[test]
    fn hint_from_the_final_error_attributes_the_failure() {
        let watch = BuildDnsWatch::default();
        let hint = watch
            .failure_hint(
                "failed to solve: failed to fetch remote https://example.com/archive.tgz: \
                 Temporary failure in name resolution",
            )
            .expect("hint from the final error");
        assert!(hint.contains("this step failed because it could not look up a host name"));
    }
}
