// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The reputation lists Shield fetches (ADR 066, D2), and parsers for their
//! formats. Parsing is pure; the control plane does the fetching.

use crate::net::Network;
use crate::snapshot::SourceList;
use crate::verdict::ListVerdict;

const DAY: u64 = 24 * 60 * 60;

/// A list Shield knows how to fetch. Every one is fetched from its
/// publisher, so attribution, timestamps and terms are the originals.
#[derive(Debug, Clone, Copy)]
pub struct Feed {
    /// The source name shown in reasons, e.g. `spamhaus_drop (1.2.3.0/24)`.
    pub name: &'static str,
    /// Fetched and merged into one list.
    pub urls: &'static [&'static str],
    pub format: Format,
    pub verdict: ListVerdict,
    /// Past this age, counted from the list's own publication time, matches
    /// are only logged: an old copy acts on networks that may have moved.
    pub max_age_secs: u64,
    /// Whether every document states when it was published. An undated
    /// list is as fresh as its last successful fetch.
    pub dated: bool,
    /// Whether a fresh install fetches it. Off for lists whose terms do not
    /// allow every use temps is put to.
    pub enabled_by_default: bool,
    pub terms: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One JSON object per line with a `cidr`, then a metadata line with the
    /// publication `timestamp`.
    SpamhausJson,
    /// Tab-separated: start, end, prefix length, …; `#` comments.
    DshieldBlock,
    /// One address or network per line; `#` comments.
    Plain,
}

pub const FEEDS: &[Feed] = &[
    Feed {
        name: "spamhaus_drop",
        urls: &[
            "https://www.spamhaus.org/drop/drop_v4.json",
            "https://www.spamhaus.org/drop/drop_v6.json",
        ],
        format: Format::SpamhausJson,
        verdict: ListVerdict::Block,
        max_age_secs: 30 * DAY,
        dated: true,
        enabled_by_default: true,
        terms: "https://www.spamhaus.org/drop/terms/",
    },
    Feed {
        name: "fullbogons",
        urls: &[
            "https://www.team-cymru.org/Services/Bogons/fullbogons-ipv4.txt",
            "https://www.team-cymru.org/Services/Bogons/fullbogons-ipv6.txt",
        ],
        format: Format::Plain,
        verdict: ListVerdict::Log,
        max_age_secs: 30 * DAY,
        dated: true,
        enabled_by_default: true,
        terms: "https://www.team-cymru.com/bogon-reference-http",
    },
    Feed {
        name: "feodo",
        urls: &["https://feodotracker.abuse.ch/downloads/ipblocklist_recommended.txt"],
        format: Format::Plain,
        verdict: ListVerdict::Block,
        max_age_secs: 7 * DAY,
        dated: false,
        enabled_by_default: true,
        // CC0 for commercial and non-commercial use (abuse.ch terms, 3.4).
        terms: "https://abuse.ch/terms-of-use/",
    },
    Feed {
        name: "dshield",
        urls: &["https://feeds.dshield.org/block.txt"],
        format: Format::DshieldBlock,
        verdict: ListVerdict::Challenge,
        max_age_secs: 3 * DAY,
        dated: false,
        // CC BY-NC-SA 2.5: not for commercial use, so an operator opts in.
        enabled_by_default: false,
        terms: "https://creativecommons.org/licenses/by-nc-sa/2.5/",
    },
];

pub fn feed(name: &str) -> Option<&'static Feed> {
    FEEDS.iter().find(|feed| feed.name == name)
}

/// What one fetched document holds.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub networks: Vec<Network>,
    /// The publication time the document states, if it states one.
    pub published_at: Option<i64>,
    /// Lines that held something but did not parse.
    pub rejected: usize,
}

pub fn parse(format: Format, text: &str) -> Parsed {
    match format {
        Format::SpamhausJson => parse_spamhaus(text),
        Format::DshieldBlock => parse_lines(text, |line| {
            let mut fields = line.split('\t');
            let start = fields.next()?;
            let _end = fields.next()?;
            let prefix = fields.next()?.trim();
            Network::parse(&format!("{}/{prefix}", start.trim()))
        }),
        Format::Plain => parse_lines(text, |line| Network::parse(line.split_whitespace().next()?)),
    }
}

fn parse_lines(text: &str, parse_line: impl Fn(&str) -> Option<Network>) -> Parsed {
    let mut parsed = Parsed::default();
    for line in text.lines() {
        let line = line.trim();
        if let Some(comment) = line.strip_prefix('#') {
            // Team Cymru: `# last updated 1792976101 (Mon Oct 26 ...)`.
            if let Some(rest) = comment.trim().strip_prefix("last updated ") {
                let stamp = rest.split_whitespace().next().and_then(|t| t.parse().ok());
                parsed.published_at = parsed.published_at.or(stamp);
            }
            continue;
        }
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        match parse_line(line) {
            Some(network) => parsed.networks.push(network),
            None => parsed.rejected += 1,
        }
    }
    parsed
}

fn parse_spamhaus(text: &str) -> Parsed {
    #[derive(serde::Deserialize)]
    struct Line {
        cidr: Option<String>,
        #[serde(rename = "type")]
        kind: Option<String>,
        timestamp: Option<i64>,
    }
    let mut parsed = Parsed::default();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(line) = serde_json::from_str::<Line>(line) else {
            parsed.rejected += 1;
            continue;
        };
        if line.kind.as_deref() == Some("metadata") {
            parsed.published_at = line.timestamp;
            continue;
        }
        match line.cidr.as_deref().and_then(Network::parse) {
            Some(network) => parsed.networks.push(network),
            None => parsed.rejected += 1,
        }
    }
    parsed
}

/// Networks broader than these are dropped from a list that acts on
/// matches: a publisher's mistake must not challenge or block a sixteenth
/// of the internet.
const MIN_ACTING_PREFIX_V4: u8 = 8;
const MIN_ACTING_PREFIX_V6: u8 = 16;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum FeedError {
    /// Mostly lines that are not networks: an error page, or a format the
    /// parser no longer understands. The last good copy is kept.
    #[error("{rejected} of {total} entries did not parse")]
    Unusable { rejected: usize, total: usize },
}

/// The list `feed`'s fetched `documents` make, dated by the documents when
/// the feed dates itself, else by `fetched_at`.
pub fn assemble(feed: &Feed, documents: &[&str], fetched_at: i64) -> Result<SourceList, FeedError> {
    let mut networks = Vec::new();
    let mut rejected = 0;
    let mut published_at: Option<i64> = None;
    for document in documents {
        let parsed = parse(feed.format, document);
        networks.extend(parsed.networks);
        rejected += parsed.rejected;
        if let Some(stamp) = parsed.published_at {
            published_at = Some(published_at.map_or(stamp, |oldest| oldest.min(stamp)));
        }
    }
    if feed.verdict != ListVerdict::Log {
        let before = networks.len();
        networks.retain(|network| match network {
            Network::V4 { prefix, .. } => *prefix >= MIN_ACTING_PREFIX_V4,
            Network::V6 { prefix, .. } => *prefix >= MIN_ACTING_PREFIX_V6,
        });
        rejected += before - networks.len();
    }
    if rejected > networks.len() {
        return Err(FeedError::Unusable {
            rejected,
            total: rejected + networks.len(),
        });
    }
    let published_at = match published_at {
        Some(stamp) if feed.dated => stamp.min(fetched_at),
        Some(_) | None => fetched_at,
    };
    Ok(SourceList::new(
        feed.name,
        feed.verdict,
        published_at,
        Some(feed.max_age_secs),
        networks,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(parsed: &Parsed) -> Vec<String> {
        parsed.networks.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn spamhaus_json_with_its_metadata_line() {
        let text = r#"{"cidr":"1.10.16.0/20","sblid":"SBL256894","rir":"apnic"}
{"cidr":"2001:678:254::/48","sblid":"SBL697648","rir":"ripencc"}
not json
{"type":"metadata","timestamp":1792833242,"size":102980,"records":2,"copyright":"(c) 2026 The Spamhaus Project SLU"}
"#;
        let parsed = parse(Format::SpamhausJson, text);
        assert_eq!(names(&parsed), ["1.10.16.0/20", "2001:678:254::/48"]);
        assert_eq!(parsed.published_at, Some(1_792_833_242));
        assert_eq!(parsed.rejected, 1);
    }

    #[test]
    fn dshield_block_list() {
        let text = "#   DShield.org Recommended Block List\n#\n\
            77.239.124.0\t77.239.124.255\t24\t338\t-\t-\t-\n\
            193.163.125.0\t193.163.125.255\t24\t337\tINTERNET-MEASUREMENT\tGB\t>>UNKNOWN<<\n";
        let parsed = parse(Format::DshieldBlock, text);
        assert_eq!(names(&parsed), ["77.239.124.0/24", "193.163.125.0/24"]);
        assert_eq!(parsed.rejected, 0);
    }

    #[test]
    fn plain_lists_of_addresses_and_networks() {
        let feodo = "#####\n# DstIP\n50.16.16.211\n# END 1 entries\n";
        assert_eq!(names(&parse(Format::Plain, feodo)), ["50.16.16.211/32"]);
        let bogons = "# last updated 1792976101 (Mon Oct 26 00:55:01 2026 GMT)\n\
            0.0.0.0/8\n2001:db8::/32\ngarbage\n";
        let parsed = parse(Format::Plain, bogons);
        assert_eq!(names(&parsed), ["0.0.0.0/8", "2001:db8::/32"]);
        assert_eq!(parsed.rejected, 1);
        assert_eq!(parsed.published_at, Some(1_792_976_101));
    }

    #[test]
    fn a_dated_feed_is_as_old_as_its_oldest_document() {
        let drop = feed("spamhaus_drop").unwrap();
        let v4 = r#"{"cidr":"1.10.16.0/20"}
{"type":"metadata","timestamp":1000}"#;
        let v6 = r#"{"cidr":"2001:678:254::/48"}
{"type":"metadata","timestamp":900}"#;
        let list = assemble(drop, &[v4, v6], 2000).unwrap();
        assert_eq!(list.published_at, 900);
        assert_eq!(list.len(), 2);
        assert_eq!(list.verdict, ListVerdict::Block);
        assert_eq!(list.max_age_secs, Some(30 * DAY));
        // A timestamp from the future is clamped to the fetch.
        let future = r#"{"cidr":"1.10.16.0/20"}
{"type":"metadata","timestamp":5000}"#;
        assert_eq!(assemble(drop, &[future], 2000).unwrap().published_at, 2000);
    }

    #[test]
    fn an_undated_feed_is_as_old_as_its_fetch() {
        let feodo = feed("feodo").unwrap();
        let list = assemble(feodo, &["# last updated 5\n50.16.16.211\n"], 2000).unwrap();
        assert_eq!(list.published_at, 2000);
        assert!(assemble(feodo, &["# END 0 entries\n"], 2000)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_error_page_or_a_too_broad_network_is_not_a_list() {
        let feodo = feed("feodo").unwrap();
        let page = "<html>\n<body>Service unavailable</body>\n</html>\n";
        assert_eq!(
            assemble(feodo, &[page], 0),
            Err(FeedError::Unusable {
                rejected: 3,
                total: 3
            })
        );
        let broad = "0.0.0.0/1\n2000::/3\n200.1.2.3\n";
        assert!(assemble(feodo, &[broad], 0).is_err());
        let list = assemble(feodo, &["0.0.0.0/1\n200.1.2.3\n210.0.0.0/8\n"], 0).unwrap();
        assert_eq!(list.len(), 2);
        // A list that only logs keeps them.
        let bogons = feed("fullbogons").unwrap();
        assert_eq!(assemble(bogons, &[broad], 0).unwrap().len(), 3);
    }

    #[test]
    fn every_feed_is_reachable_by_name() {
        for feed in FEEDS {
            assert_eq!(super::feed(feed.name).map(|f| f.name), Some(feed.name));
            assert!(!feed.urls.is_empty());
            assert!(feed.urls.iter().all(|url| url.starts_with("https://")));
        }
        assert!(!feed("dshield").unwrap().enabled_by_default);
    }
}
