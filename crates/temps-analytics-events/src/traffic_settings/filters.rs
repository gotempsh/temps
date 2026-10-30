// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project traffic filters: referrer domains, IPs/CIDR ranges, networks
//! (ASNs) and countries whose traffic is kept out of a project's analytics.
//!
//! Rules are written rarely from the console and read on the ingest path, so
//! ingest never queries them per event: each project's rules are compiled into
//! a [`CompiledTrafficFilters`] (hash sets plus a short CIDR list) and cached
//! for [`FILTER_CACHE_TTL`]. A write invalidates this process's entry at once;
//! any other process (split topology) picks it up within the TTL.
//!
//! A matching event is not dropped. It is stored with `is_crawler = true` and
//! `crawler_name = "rule:<kind>"`, which every analytics read on both the
//! Postgres and ClickHouse backends already excludes, and which the
//! filtered-traffic summary groups by. Deleting the rule makes new traffic
//! count again; it does not rewrite events already flagged.

use std::borrow::Cow;
use std::collections::HashSet;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ipnet::IpNet;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseBackend,
    DatabaseConnection, DbErr, EntityTrait, FromQueryResult, ModelTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, SqlErr, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use temps_entities::analytics_traffic_filters;
use thiserror::Error;
use tracing::warn;
use utoipa::ToSchema;

use super::ingest_cache::IngestCache;
use super::service::project_exists;
use crate::services::DATACENTER_CRAWLER_NAME;

/// Most rules a project may have of each kind. Bounds the compiled matcher's
/// memory (and the CIDR scan, which is linear) on the ingest path.
pub const MAX_FILTERS_PER_KIND: u64 = 500;

/// Broadest IPv4 range a rule may cover. A `/7` or wider is a typo far more
/// often than an intent, and would silently hide a large slice of the internet.
pub const MIN_IPV4_PREFIX: u8 = 8;

/// Broadest IPv6 range a rule may cover (a `/32` is a typical ISP allocation).
pub const MIN_IPV6_PREFIX: u8 = 32;

/// Longest note a rule may carry.
pub const MAX_NOTE_LEN: usize = 500;

/// `crawler_name` of events excluded by the bundled referrer-spam list.
pub const SPAM_REFERRER_CRAWLER_NAME: &str = "spam-referrer";

/// How long a project's compiled rules stay cached for the ingest path.
const FILTER_CACHE_TTL: Duration = Duration::from_secs(30);

/// Upper bound on the compiled rules cached for the ingest path, in rules
/// (see [`CompiledTrafficFilters::cache_weight`]). A compiled rule measured
/// about 33 bytes, so this holds the cache near 16 MB: every project with a
/// handful of rules, or about 250 projects at [`MAX_FILTERS_PER_KIND`] of
/// every kind at once.
const FILTER_CACHE_MAX_RULES: u64 = 500_000;

/// First key of the advisory lock that serializes rule creation per project
/// (the second key is the project id). An arbitrary constant, unique among
/// the advisory locks Temps takes.
const FILTER_WRITE_LOCK_CLASS: i32 = 0x7466_6c74;

/// Longest DNS hostname.
const MAX_HOSTNAME_LEN: usize = 253;

/// Longest window the filtered-traffic summary covers. Bounds the scan.
pub const MAX_FILTERED_TRAFFIC_DAYS: u32 = 30;

/// What a traffic filter matches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TrafficFilterKind {
    /// A referrer domain; also matches its subdomains.
    Referrer,
    /// A single IP address or a CIDR range.
    Ip,
    /// An autonomous system (network) number.
    Asn,
    /// An ISO 3166-1 alpha-2 country code.
    Country,
}

impl TrafficFilterKind {
    pub const ALL: [TrafficFilterKind; 4] = [
        TrafficFilterKind::Referrer,
        TrafficFilterKind::Ip,
        TrafficFilterKind::Asn,
        TrafficFilterKind::Country,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TrafficFilterKind::Referrer => "referrer",
            TrafficFilterKind::Ip => "ip",
            TrafficFilterKind::Asn => "asn",
            TrafficFilterKind::Country => "country",
        }
    }

    /// The `crawler_name` an event excluded by a rule of this kind is stored
    /// with. A closed vocabulary, so the column's cardinality stays bounded.
    pub fn crawler_name(self) -> &'static str {
        match self {
            TrafficFilterKind::Referrer => "rule:referrer",
            TrafficFilterKind::Ip => "rule:ip",
            TrafficFilterKind::Asn => "rule:asn",
            TrafficFilterKind::Country => "rule:country",
        }
    }
}

impl std::fmt::Display for TrafficFilterKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TrafficFilterKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "referrer" => Ok(TrafficFilterKind::Referrer),
            "ip" => Ok(TrafficFilterKind::Ip),
            "asn" => Ok(TrafficFilterKind::Asn),
            "country" => Ok(TrafficFilterKind::Country),
            other => Err(format!(
                "unknown traffic filter kind '{other}' (expected referrer, ip, asn or country)"
            )),
        }
    }
}

/// A stored traffic filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrafficFilter {
    pub id: i32,
    pub project_id: i32,
    pub kind: TrafficFilterKind,
    pub value: String,
    pub note: Option<String>,
    pub created_by: Option<i32>,
    pub created_at: DateTime<Utc>,
}

impl TrafficFilter {
    fn from_model(model: analytics_traffic_filters::Model) -> Result<Self, TrafficFilterError> {
        let kind = model
            .kind
            .parse()
            .map_err(|reason| TrafficFilterError::CorruptRow {
                filter_id: model.id,
                project_id: model.project_id,
                reason,
            })?;
        Ok(Self {
            id: model.id,
            project_id: model.project_id,
            kind,
            value: model.value,
            note: model.note,
            created_by: model.created_by,
            created_at: model.created_at,
        })
    }
}

/// A rule to create. `value` is normalised by the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTrafficFilter {
    pub kind: TrafficFilterKind,
    pub value: String,
    pub note: Option<String>,
}

/// Every `crawler_name` a traffic filter (rather than a bot user agent)
/// stores. A visitor hidden with one of these may be shown again. Defined in
/// `temps-entities` so reports in crates that do not depend on this one leave
/// out the same values.
pub const EXCLUSION_CRAWLER_NAMES: [&str; 6] = temps_entities::events::TRAFFIC_FILTER_CRAWLER_NAMES;

/// Why an event was kept out of a project's analytics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilteredTrafficReason {
    /// The user agent identified a bot or crawler.
    Bot,
    /// The IP belongs to a hosting/cloud network.
    Datacenter,
    /// The referrer is on the bundled spam list.
    SpamReferrer,
    /// A referrer rule matched.
    ReferrerRule,
    /// An IP or CIDR rule matched.
    IpRule,
    /// A network (ASN) rule matched.
    AsnRule,
    /// A country rule matched.
    CountryRule,
}

impl FilteredTrafficReason {
    /// Classify a flagged event by its `crawler_name`. Anything outside the
    /// closed exclusion vocabulary is a user-agent bot name.
    pub fn from_crawler_name(crawler_name: Option<&str>) -> Self {
        match crawler_name {
            Some(name) if name == DATACENTER_CRAWLER_NAME => FilteredTrafficReason::Datacenter,
            Some(name) if name == SPAM_REFERRER_CRAWLER_NAME => FilteredTrafficReason::SpamReferrer,
            Some("rule:referrer") => FilteredTrafficReason::ReferrerRule,
            Some("rule:ip") => FilteredTrafficReason::IpRule,
            Some("rule:asn") => FilteredTrafficReason::AsnRule,
            Some("rule:country") => FilteredTrafficReason::CountryRule,
            _ => FilteredTrafficReason::Bot,
        }
    }
}

/// Events counted and kept out of a project's analytics over a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredTrafficSummary {
    pub project_id: i32,
    pub days: u32,
    pub since: DateTime<Utc>,
    /// Events that count towards the project's analytics.
    pub counted_events: i64,
    /// Excluded events per reason, largest first. Reasons with no events are
    /// omitted.
    pub filtered: Vec<FilteredTrafficCount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredTrafficCount {
    pub reason: FilteredTrafficReason,
    pub events: i64,
}

#[derive(Error, Debug)]
pub enum TrafficFilterError {
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Traffic filter {filter_id} not found in project {project_id}")]
    FilterNotFound { filter_id: i32, project_id: i32 },

    #[error("Invalid {kind} filter '{value}': {reason}")]
    InvalidValue {
        kind: TrafficFilterKind,
        value: String,
        reason: String,
    },

    #[error("Invalid traffic filter request: {message}")]
    Validation { message: String },

    #[error(
        "Project {project_id} already has {limit} {kind} filters, the most allowed; remove one first"
    )]
    LimitReached {
        project_id: i32,
        kind: TrafficFilterKind,
        limit: u64,
    },

    #[error("Project {project_id} already has a {kind} filter for '{value}'")]
    Duplicate {
        project_id: i32,
        kind: TrafficFilterKind,
        value: String,
    },

    #[error("Traffic filter {filter_id} in project {project_id} is unreadable: {reason}")]
    CorruptRow {
        filter_id: i32,
        project_id: i32,
        reason: String,
    },

    #[error("Database error while {operation} traffic filters for project {project_id}: {source}")]
    Database {
        project_id: i32,
        operation: &'static str,
        #[source]
        source: DbErr,
    },
}

/// `::ffff:a.b.c.d` as `a.b.c.d`; every other address unchanged.
fn unmap_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

/// A range inside the IPv4-mapped block (`::ffff:0:0/96`) as the IPv4 range
/// it covers; every other range unchanged.
fn unmap_net(net: IpNet) -> IpNet {
    if let IpNet::V6(v6) = net {
        if v6.prefix_len() >= 96 {
            if let Some(v4) = v6.network().to_ipv4_mapped() {
                if let Ok(v4_net) = ipnet::Ipv4Net::new(v4, v6.prefix_len() - 96) {
                    return IpNet::V4(v4_net);
                }
            }
        }
    }
    net
}

/// A referrer host as rules and the spam list are written: lowercase, without
/// the trailing dot of a fully qualified name. Borrows when already in that
/// form, which is every host a browser sends.
pub fn canonical_referrer_host(host: &str) -> Cow<'_, str> {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(host.to_ascii_lowercase())
    } else {
        Cow::Borrowed(host)
    }
}

/// A two-letter country code as a case-folded set key, so matching an event
/// needs neither an allocation nor a scan. `None` for anything else.
fn country_key(code: &str) -> Option<[u8; 2]> {
    match code.as_bytes() {
        [a, b] if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() => {
            Some([a.to_ascii_uppercase(), b.to_ascii_uppercase()])
        }
        _ => None,
    }
}

/// The `host`, then each parent domain, ending at the top-level label:
/// `a.b.example.com` → `a.b.example.com`, `b.example.com`, `example.com`,
/// `com`. Matching suffixes by whole label is what keeps `notexample.com` from
/// matching a rule for `example.com`.
pub(crate) fn host_suffixes(host: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(host), |current| {
        current.split_once('.').map(|(_, parent)| parent)
    })
    .filter(|suffix| !suffix.is_empty())
}

/// Normalise and validate a rule value, so equal rules are stored equal.
///
/// - `referrer`: a hostname; a pasted URL, `*.` prefix, `www.` prefix, port
///   and trailing dot are stripped. Matches the host and its subdomains.
/// - `ip`: an address, or a CIDR range no broader than `/8` (IPv4) or `/32`
///   (IPv6), stored with host bits cleared. A `/32` or `/128` is stored as a
///   plain address.
/// - `asn`: a number, with or without an `AS` prefix.
/// - `country`: an ISO 3166-1 alpha-2 code, stored uppercase.
pub fn normalize_filter_value(
    kind: TrafficFilterKind,
    raw: &str,
) -> Result<String, TrafficFilterError> {
    let invalid = |reason: &str| TrafficFilterError::InvalidValue {
        kind,
        value: raw.to_string(),
        reason: reason.to_string(),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid("a value is required"));
    }

    match kind {
        TrafficFilterKind::Referrer => normalize_referrer_host(trimmed).map_err(|r| invalid(&r)),
        TrafficFilterKind::Ip => {
            // Rules are stored in IPv4 form for IPv4-mapped input
            // (`::ffff:203.0.113.7`), because that is the form the matcher
            // compares client addresses in.
            if let Ok(ip) = trimmed.parse::<IpAddr>() {
                return Ok(unmap_ip(ip).to_string());
            }
            let net: IpNet = trimmed.parse().map_err(|_| {
                invalid("expected an IP address (203.0.113.7) or a CIDR range (203.0.113.0/24)")
            })?;
            let net = unmap_net(net);
            let (min_prefix, max_prefix) = match net {
                IpNet::V4(_) => (MIN_IPV4_PREFIX, 32),
                IpNet::V6(_) => (MIN_IPV6_PREFIX, 128),
            };
            if net.prefix_len() < min_prefix {
                return Err(invalid(&format!(
                    "a /{} range is too broad; the broadest allowed is /{}",
                    net.prefix_len(),
                    min_prefix
                )));
            }
            if net.prefix_len() == max_prefix {
                return Ok(net.addr().to_string());
            }
            Ok(net.trunc().to_string())
        }
        TrafficFilterKind::Asn => {
            let digits = trimmed
                .strip_prefix("AS")
                .or_else(|| trimmed.strip_prefix("as"))
                .or_else(|| trimmed.strip_prefix("As"))
                .unwrap_or(trimmed)
                .trim();
            match digits.parse::<u32>() {
                Ok(0) | Err(_) => Err(invalid(
                    "expected a network (autonomous system) number such as 132203 or AS132203",
                )),
                Ok(asn) => Ok(asn.to_string()),
            }
        }
        TrafficFilterKind::Country => {
            if trimmed.len() == 2 && trimmed.chars().all(|c| c.is_ascii_alphabetic()) {
                Ok(trimmed.to_ascii_uppercase())
            } else {
                Err(invalid(
                    "expected a two-letter ISO 3166-1 country code such as SG",
                ))
            }
        }
    }
}

fn normalize_referrer_host(raw: &str) -> Result<String, String> {
    let lowered = raw.to_ascii_lowercase();
    let mut host = lowered.as_str();
    if let Some((_, rest)) = host.split_once("://") {
        host = rest;
    }
    if let Some(end) = host.find(['/', '?', '#']) {
        host = &host[..end];
    }
    if let Some((_, rest)) = host.rsplit_once('@') {
        host = rest;
    }
    if let Some((name, port)) = host.rsplit_once(':') {
        if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
            host = name;
        }
    }
    host = host.strip_prefix("*.").unwrap_or(host);
    host = host.strip_prefix("www.").unwrap_or(host);
    host = host.trim_end_matches('.');

    if host.is_empty() {
        return Err("expected a domain such as spam-site.example".to_string());
    }
    if host.len() > MAX_HOSTNAME_LEN {
        return Err(format!("a domain is at most {MAX_HOSTNAME_LEN} characters"));
    }
    if !host.contains('.') {
        return Err(
            "expected a full domain such as spam-site.example, not a bare name".to_string(),
        );
    }
    for label in host.split('.') {
        let valid = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(format!(
                "'{label}' is not a valid domain label (letters, digits and hyphens only)"
            ));
        }
    }
    Ok(host.to_string())
}

/// Request attributes a traffic filter can match on, borrowed from the event
/// being ingested.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrafficAttributes<'a> {
    /// Referrer hostname, as extracted for the event row.
    pub referrer_host: Option<&'a str>,
    pub ip: Option<IpAddr>,
    pub asn_number: Option<u32>,
    pub country_code: Option<&'a str>,
}

/// A project's rules, compiled for constant-time matching (CIDR ranges are a
/// linear scan over at most [`MAX_FILTERS_PER_KIND`] entries).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompiledTrafficFilters {
    referrers: HashSet<String>,
    ips: HashSet<IpAddr>,
    networks: Vec<IpNet>,
    asns: HashSet<u32>,
    countries: HashSet<[u8; 2]>,
}

impl CompiledTrafficFilters {
    /// Compile stored `(kind, value)` pairs. A value that no longer parses
    /// (the validation rules only ever tighten) is skipped and logged rather
    /// than failing every event of the project.
    pub fn compile<'a>(
        project_id: i32,
        rules: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Self {
        let mut compiled = Self::default();
        for (kind, value) in rules {
            let ok = match kind.parse::<TrafficFilterKind>() {
                Ok(TrafficFilterKind::Referrer) => {
                    compiled.referrers.insert(value.to_string());
                    true
                }
                Ok(TrafficFilterKind::Ip) => {
                    if let Ok(ip) = value.parse::<IpAddr>() {
                        compiled.ips.insert(unmap_ip(ip));
                        true
                    } else if let Ok(net) = value.parse::<IpNet>() {
                        compiled.networks.push(unmap_net(net));
                        true
                    } else {
                        false
                    }
                }
                Ok(TrafficFilterKind::Asn) => value
                    .parse::<u32>()
                    .map(|asn| compiled.asns.insert(asn))
                    .is_ok(),
                Ok(TrafficFilterKind::Country) => country_key(value)
                    .map(|key| compiled.countries.insert(key))
                    .is_some(),
                Err(_) => false,
            };
            if !ok {
                warn!(
                    project_id,
                    kind, value, "skipping an unreadable analytics traffic filter"
                );
            }
        }
        compiled
    }

    /// Weight of this rule set in the ingest cache: one per rule, plus one so
    /// a project without rules still counts against the bound.
    fn cache_weight(&self) -> u32 {
        let rules = self.referrers.len()
            + self.ips.len()
            + self.networks.len()
            + self.asns.len()
            + self.countries.len();
        u32::try_from(rules).map_or(u32::MAX, |rules| rules.saturating_add(1))
    }

    pub fn is_empty(&self) -> bool {
        self.referrers.is_empty()
            && self.ips.is_empty()
            && self.networks.is_empty()
            && self.asns.is_empty()
            && self.countries.is_empty()
    }

    /// The kind of the first rule `traffic` matches, if any.
    pub fn matching_kind(&self, traffic: &TrafficAttributes<'_>) -> Option<TrafficFilterKind> {
        if self.is_empty() {
            return None;
        }
        if !self.referrers.is_empty() {
            if let Some(host) = traffic.referrer_host {
                let host = canonical_referrer_host(host);
                if host_suffixes(&host).any(|suffix| self.referrers.contains(suffix)) {
                    return Some(TrafficFilterKind::Referrer);
                }
            }
        }
        if let Some(ip) = traffic.ip {
            // An IPv4 client seen over a dual-stack socket arrives as
            // `::ffff:a.b.c.d`; rules are written against the IPv4 form.
            let ip = unmap_ip(ip);
            if self.ips.contains(&ip) || self.networks.iter().any(|net| net.contains(&ip)) {
                return Some(TrafficFilterKind::Ip);
            }
        }
        if let Some(asn) = traffic.asn_number {
            if self.asns.contains(&asn) {
                return Some(TrafficFilterKind::Asn);
            }
        }
        if let Some(country) = traffic.country_code {
            if country_key(country).is_some_and(|key| self.countries.contains(&key)) {
                return Some(TrafficFilterKind::Country);
            }
        }
        None
    }
}

#[derive(Debug, FromQueryResult)]
struct FilteredTrafficRow {
    is_crawler: bool,
    crawler_name: Option<String>,
    events: i64,
}

pub struct TrafficFilterService {
    db: Arc<DatabaseConnection>,
    cache: IngestCache<Arc<CompiledTrafficFilters>>,
}

impl TrafficFilterService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        let cache = IngestCache::new(
            FILTER_CACHE_TTL,
            FILTER_CACHE_MAX_RULES,
            |filters: &Arc<CompiledTrafficFilters>| filters.cache_weight(),
        );
        Self { db, cache }
    }

    /// A page of the project's rules, newest first, optionally of one kind.
    /// Returns the rules and the total matching count.
    pub async fn list(
        &self,
        project_id: i32,
        kind: Option<TrafficFilterKind>,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<TrafficFilter>, u64), TrafficFilterError> {
        Self::ensure_project_exists(self.db.as_ref(), project_id).await?;
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size.unwrap_or(20).clamp(1, 100);

        let mut query = analytics_traffic_filters::Entity::find()
            .filter(analytics_traffic_filters::Column::ProjectId.eq(project_id));
        if let Some(kind) = kind {
            query = query.filter(analytics_traffic_filters::Column::Kind.eq(kind.as_str()));
        }
        let paginator = query
            .order_by_desc(analytics_traffic_filters::Column::CreatedAt)
            .order_by_desc(analytics_traffic_filters::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator
            .num_items()
            .await
            .map_err(|source| TrafficFilterError::Database {
                project_id,
                operation: "counting",
                source,
            })?;
        let rows = paginator.fetch_page(page - 1).await.map_err(|source| {
            TrafficFilterError::Database {
                project_id,
                operation: "listing",
                source,
            }
        })?;
        let filters = rows
            .into_iter()
            .map(TrafficFilter::from_model)
            .collect::<Result<Vec<_>, _>>()?;
        Ok((filters, total))
    }

    /// Add a rule. Takes effect on this process's next event for the project,
    /// and on other processes within [`FILTER_CACHE_TTL`].
    pub async fn create(
        &self,
        project_id: i32,
        new_filter: NewTrafficFilter,
        created_by: Option<i32>,
    ) -> Result<TrafficFilter, TrafficFilterError> {
        let kind = new_filter.kind;
        let value = normalize_filter_value(kind, &new_filter.value)?;
        let note = match new_filter.note.map(|n| n.trim().to_string()) {
            Some(note) if note.chars().count() > MAX_NOTE_LEN => {
                return Err(TrafficFilterError::Validation {
                    message: format!("a note is at most {MAX_NOTE_LEN} characters"),
                });
            }
            Some(note) if note.is_empty() => None,
            other => other,
        };

        let db_error = |operation: &'static str| {
            move |source: DbErr| TrafficFilterError::Database {
                project_id,
                operation,
                source,
            }
        };

        // The per-kind limit is a count followed by an insert. Two concurrent
        // creates of different values would both see room for one more and
        // both insert, so creates for one project are serialized for the
        // length of this transaction. An advisory lock rather than a lock on
        // the project row, which unrelated project writes would queue behind.
        let txn = self.db.begin().await.map_err(db_error("creating"))?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1, $2)",
            [FILTER_WRITE_LOCK_CLASS.into(), project_id.into()],
        ))
        .await
        .map_err(db_error("locking"))?;

        Self::ensure_project_exists(&txn, project_id).await?;

        // Checked up front for a deterministic error; the unique constraint
        // still backs it up.
        let duplicate = analytics_traffic_filters::Entity::find()
            .filter(analytics_traffic_filters::Column::ProjectId.eq(project_id))
            .filter(analytics_traffic_filters::Column::Kind.eq(kind.as_str()))
            .filter(analytics_traffic_filters::Column::Value.eq(value.as_str()))
            .one(&txn)
            .await
            .map_err(db_error("checking for duplicate"))?;
        if duplicate.is_some() {
            return Err(TrafficFilterError::Duplicate {
                project_id,
                kind,
                value,
            });
        }

        let existing = analytics_traffic_filters::Entity::find()
            .filter(analytics_traffic_filters::Column::ProjectId.eq(project_id))
            .filter(analytics_traffic_filters::Column::Kind.eq(kind.as_str()))
            .count(&txn)
            .await
            .map_err(db_error("counting"))?;
        if existing >= MAX_FILTERS_PER_KIND {
            return Err(TrafficFilterError::LimitReached {
                project_id,
                kind,
                limit: MAX_FILTERS_PER_KIND,
            });
        }

        let row = analytics_traffic_filters::ActiveModel {
            project_id: Set(project_id),
            kind: Set(kind.as_str().to_string()),
            value: Set(value.clone()),
            note: Set(note),
            created_by: Set(created_by),
            created_at: Set(Utc::now()),
            ..Default::default()
        };
        let saved = row.insert(&txn).await.map_err(|source| {
            if matches!(source.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) {
                TrafficFilterError::Duplicate {
                    project_id,
                    kind,
                    value: value.clone(),
                }
            } else {
                TrafficFilterError::Database {
                    project_id,
                    operation: "creating",
                    source,
                }
            }
        })?;
        txn.commit().await.map_err(db_error("creating"))?;

        self.cache.invalidate(project_id).await;
        TrafficFilter::from_model(saved)
    }

    /// Remove a rule and return it. New traffic it matched counts again;
    /// events it already flagged stay excluded.
    pub async fn delete(
        &self,
        project_id: i32,
        filter_id: i32,
    ) -> Result<TrafficFilter, TrafficFilterError> {
        let row = analytics_traffic_filters::Entity::find_by_id(filter_id)
            .filter(analytics_traffic_filters::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|source| TrafficFilterError::Database {
                project_id,
                operation: "loading",
                source,
            })?
            .ok_or(TrafficFilterError::FilterNotFound {
                filter_id,
                project_id,
            })?;

        row.clone()
            .delete(self.db.as_ref())
            .await
            .map_err(|source| TrafficFilterError::Database {
                project_id,
                operation: "deleting",
                source,
            })?;

        self.cache.invalidate(project_id).await;
        TrafficFilter::from_model(row)
    }

    /// The project's compiled rules, for the ingest path. Served from the
    /// cache and never fails: on a database error the event is matched
    /// against no rules (and so counted) and the error is logged, because
    /// failing ingest over a filter lookup would lose real traffic too.
    pub async fn compiled(&self, project_id: i32) -> Arc<CompiledTrafficFilters> {
        let db = &self.db;
        let loaded = self
            .cache
            .get_or_load(project_id, || async move {
                let rows: Vec<(String, String)> = analytics_traffic_filters::Entity::find()
                    .select_only()
                    .column(analytics_traffic_filters::Column::Kind)
                    .column(analytics_traffic_filters::Column::Value)
                    .filter(analytics_traffic_filters::Column::ProjectId.eq(project_id))
                    .into_tuple()
                    .all(db.as_ref())
                    .await?;
                Ok(Arc::new(CompiledTrafficFilters::compile(
                    project_id,
                    rows.iter()
                        .map(|(kind, value)| (kind.as_str(), value.as_str())),
                )))
            })
            .await;
        match loaded {
            Ok(compiled) => compiled,
            Err(e) => {
                warn!(
                    project_id,
                    error = %e,
                    "could not load analytics traffic filters; counting this event unfiltered"
                );
                Arc::new(CompiledTrafficFilters::default())
            }
        }
    }

    /// Events counted and excluded over the last `days` days, by reason.
    pub async fn filtered_traffic(
        &self,
        project_id: i32,
        days: u32,
    ) -> Result<FilteredTrafficSummary, TrafficFilterError> {
        if days == 0 || days > MAX_FILTERED_TRAFFIC_DAYS {
            return Err(TrafficFilterError::Validation {
                message: format!(
                    "days must be between 1 and {MAX_FILTERED_TRAFFIC_DAYS}, got {days}"
                ),
            });
        }
        Self::ensure_project_exists(self.db.as_ref(), project_id).await?;

        let since = Utc::now() - chrono::Duration::days(i64::from(days));
        // Grouped by the raw `crawler_name`, whose cardinality is bounded by
        // the UA bot list plus the closed exclusion vocabulary; the reason is
        // derived in Rust so the mapping lives in one place.
        let rows = FilteredTrafficRow::find_by_statement(Statement::from_sql_and_values(
            self.db.get_database_backend(),
            r#"
            SELECT is_crawler,
                   CASE WHEN is_crawler THEN crawler_name END AS crawler_name,
                   COUNT(*)::bigint AS events
            FROM events
            WHERE project_id = $1 AND timestamp >= $2
            GROUP BY 1, 2
            "#,
            [project_id.into(), since.into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| TrafficFilterError::Database {
            project_id,
            operation: "summarising filtered events for",
            source,
        })?;

        let mut counted_events = 0;
        let mut by_reason: Vec<FilteredTrafficCount> = Vec::new();
        for row in rows {
            if !row.is_crawler {
                counted_events += row.events;
                continue;
            }
            let reason = FilteredTrafficReason::from_crawler_name(row.crawler_name.as_deref());
            match by_reason.iter_mut().find(|count| count.reason == reason) {
                Some(count) => count.events += row.events,
                None => by_reason.push(FilteredTrafficCount {
                    reason,
                    events: row.events,
                }),
            }
        }
        by_reason.sort_by_key(|count| std::cmp::Reverse(count.events));

        Ok(FilteredTrafficSummary {
            project_id,
            days,
            since,
            counted_events,
            filtered: by_reason,
        })
    }

    async fn ensure_project_exists<C: ConnectionTrait>(
        db: &C,
        project_id: i32,
    ) -> Result<(), TrafficFilterError> {
        match project_exists(db, project_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(TrafficFilterError::ProjectNotFound { project_id }),
            Err(source) => Err(TrafficFilterError::Database {
                project_id,
                operation: "checking the project for",
                source,
            }),
        }
    }
}

#[cfg(test)]
pub(in crate::traffic_settings) mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    pub(in crate::traffic_settings) fn lock_acquired() -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("test IP")
    }

    // --- normalisation -------------------------------------------------

    #[test]
    fn referrer_values_are_reduced_to_a_bare_lowercase_host() {
        for raw in [
            "spam-site.example",
            "SPAM-SITE.example",
            "https://spam-site.example/path?q=1#x",
            "http://www.spam-site.example:8080/",
            "*.spam-site.example",
            "spam-site.example.",
            "  spam-site.example  ",
        ] {
            assert_eq!(
                normalize_filter_value(TrafficFilterKind::Referrer, raw).expect(raw),
                "spam-site.example",
                "normalising {raw:?}"
            );
        }
    }

    #[test]
    fn invalid_referrers_are_rejected_with_the_value_in_the_message() {
        for raw in [
            "",
            "   ",
            "localhost",
            "bad domain.example",
            "-x.example",
            "https://",
        ] {
            let err = normalize_filter_value(TrafficFilterKind::Referrer, raw)
                .expect_err(&format!("{raw:?} must be rejected"));
            assert!(
                matches!(
                    err,
                    TrafficFilterError::InvalidValue {
                        kind: TrafficFilterKind::Referrer,
                        ..
                    }
                ),
                "{raw:?}: {err}"
            );
        }
        let err = normalize_filter_value(TrafficFilterKind::Referrer, "localhost").unwrap_err();
        assert!(err.to_string().contains("'localhost'"));
    }

    #[test]
    fn ip_values_are_canonicalised() {
        let n = |raw| normalize_filter_value(TrafficFilterKind::Ip, raw);
        assert_eq!(n("203.0.113.7").unwrap(), "203.0.113.7");
        assert_eq!(n("203.0.113.77/24").unwrap(), "203.0.113.0/24");
        assert_eq!(n("203.0.113.7/32").unwrap(), "203.0.113.7");
        assert_eq!(n("2001:DB8::1").unwrap(), "2001:db8::1");
        assert_eq!(n("2001:db8:abcd::1/48").unwrap(), "2001:db8:abcd::/48");
        assert_eq!(n("10.0.0.0/8").unwrap(), "10.0.0.0/8");
    }

    /// An address pasted in IPv4-mapped form must be stored the way the
    /// matcher compares clients (IPv4), or the rule is saved and never fires.
    #[test]
    fn ipv4_mapped_rules_are_stored_and_matched_as_ipv4() {
        let n = |raw| normalize_filter_value(TrafficFilterKind::Ip, raw);
        assert_eq!(n("::ffff:203.0.113.5").unwrap(), "203.0.113.5");
        assert_eq!(n("::ffff:203.0.113.0/120").unwrap(), "203.0.113.0/24");
        assert_eq!(n("::ffff:203.0.113.5/128").unwrap(), "203.0.113.5");
        // Broader than /8 once unmapped: refused like the IPv4 form.
        assert!(n("::ffff:0.0.0.0/100").is_err());

        // A mapped rule stored before this normalisation still matches.
        let rules = compiled(&[
            ("ip", "::ffff:203.0.113.5"),
            ("ip", "::ffff:198.51.100.0/120"),
        ]);
        for client in ["::ffff:203.0.113.5", "203.0.113.5", "198.51.100.9"] {
            assert_eq!(
                rules.matching_kind(&TrafficAttributes {
                    ip: Some(ip(client)),
                    ..Default::default()
                }),
                Some(TrafficFilterKind::Ip),
                "{client}"
            );
        }
    }

    #[test]
    fn overly_broad_or_malformed_ranges_are_rejected() {
        for raw in [
            "0.0.0.0/0",
            "10.0.0.0/7",
            "2001::/16",
            "not-an-ip",
            "300.1.1.1",
        ] {
            assert!(
                normalize_filter_value(TrafficFilterKind::Ip, raw).is_err(),
                "{raw} must be rejected"
            );
        }
        let err = normalize_filter_value(TrafficFilterKind::Ip, "10.0.0.0/7").unwrap_err();
        assert!(err.to_string().contains("too broad"), "{err}");
    }

    #[test]
    fn asn_values_accept_an_as_prefix() {
        let n = |raw| normalize_filter_value(TrafficFilterKind::Asn, raw);
        assert_eq!(n("132203").unwrap(), "132203");
        assert_eq!(n("AS132203").unwrap(), "132203");
        assert_eq!(n("as 132203").unwrap(), "132203");
        assert!(n("0").is_err());
        assert!(n("AS").is_err());
        assert!(n("-5").is_err());
        assert!(n("99999999999").is_err());
    }

    #[test]
    fn country_values_are_two_letter_codes() {
        let n = |raw| normalize_filter_value(TrafficFilterKind::Country, raw);
        assert_eq!(n("sg").unwrap(), "SG");
        assert_eq!(n(" SG ").unwrap(), "SG");
        assert!(n("SGP").is_err());
        assert!(n("S1").is_err());
    }

    #[test]
    fn host_suffixes_walk_whole_labels() {
        assert_eq!(
            host_suffixes("a.b.example.com").collect::<Vec<_>>(),
            vec!["a.b.example.com", "b.example.com", "example.com", "com"]
        );
        assert_eq!(host_suffixes("").count(), 0);
    }

    #[test]
    fn crawler_names_round_trip_to_reasons() {
        for kind in TrafficFilterKind::ALL {
            let reason = FilteredTrafficReason::from_crawler_name(Some(kind.crawler_name()));
            assert_ne!(reason, FilteredTrafficReason::Bot, "{kind}");
        }
        assert_eq!(
            FilteredTrafficReason::from_crawler_name(Some(DATACENTER_CRAWLER_NAME)),
            FilteredTrafficReason::Datacenter
        );
        assert_eq!(
            FilteredTrafficReason::from_crawler_name(Some(SPAM_REFERRER_CRAWLER_NAME)),
            FilteredTrafficReason::SpamReferrer
        );
        assert_eq!(
            FilteredTrafficReason::from_crawler_name(Some("Googlebot")),
            FilteredTrafficReason::Bot
        );
        assert_eq!(
            FilteredTrafficReason::from_crawler_name(None),
            FilteredTrafficReason::Bot
        );
    }

    // --- matching ------------------------------------------------------

    fn compiled(rules: &[(&str, &str)]) -> CompiledTrafficFilters {
        CompiledTrafficFilters::compile(1, rules.iter().copied())
    }

    #[test]
    fn cache_weight_counts_rules_so_large_rule_sets_weigh_more() {
        assert_eq!(compiled(&[]).cache_weight(), 1, "an empty set still counts");
        let rules = compiled(&[
            ("referrer", "spam-site.example"),
            ("ip", "203.0.113.7"),
            ("ip", "198.51.100.0/24"),
            ("asn", "64500"),
            ("country", "SG"),
        ]);
        assert_eq!(rules.cache_weight(), 6);
    }

    #[test]
    fn a_referrer_rule_matches_the_domain_and_subdomains_only() {
        let rules = compiled(&[("referrer", "spam-site.example")]);
        let hit = |host| {
            rules.matching_kind(&TrafficAttributes {
                referrer_host: Some(host),
                ..Default::default()
            })
        };
        assert_eq!(hit("spam-site.example"), Some(TrafficFilterKind::Referrer));
        assert_eq!(
            hit("www.spam-site.example"),
            Some(TrafficFilterKind::Referrer)
        );
        assert_eq!(
            hit("WWW.Spam-Site.Example"),
            Some(TrafficFilterKind::Referrer)
        );
        assert_eq!(hit("notspam-site.example"), None);
        assert_eq!(hit("example"), None);
    }

    #[test]
    fn ip_rules_match_addresses_ranges_and_mapped_ipv4() {
        let rules = compiled(&[("ip", "198.51.100.9"), ("ip", "203.0.113.0/24")]);
        let hit = |addr: &str| {
            rules.matching_kind(&TrafficAttributes {
                ip: Some(ip(addr)),
                ..Default::default()
            })
        };
        assert_eq!(hit("198.51.100.9"), Some(TrafficFilterKind::Ip));
        assert_eq!(hit("203.0.113.200"), Some(TrafficFilterKind::Ip));
        assert_eq!(hit("::ffff:203.0.113.5"), Some(TrafficFilterKind::Ip));
        assert_eq!(hit("198.51.100.10"), None);
        assert_eq!(hit("203.0.114.1"), None);
    }

    #[test]
    fn asn_and_country_rules_match() {
        let rules = compiled(&[("asn", "132203"), ("country", "SG")]);
        assert_eq!(
            rules.matching_kind(&TrafficAttributes {
                asn_number: Some(132203),
                ..Default::default()
            }),
            Some(TrafficFilterKind::Asn)
        );
        assert_eq!(
            rules.matching_kind(&TrafficAttributes {
                country_code: Some("sg"),
                ..Default::default()
            }),
            Some(TrafficFilterKind::Country)
        );
        assert_eq!(
            rules.matching_kind(&TrafficAttributes {
                asn_number: Some(16509),
                country_code: Some("US"),
                ..Default::default()
            }),
            None
        );
    }

    /// Recovery un-hides only visitors whose stored reason is in this list,
    /// so it must cover every reason a filter writes and never a bot name.
    #[test]
    fn exclusion_crawler_names_cover_every_filter_reason() {
        for name in EXCLUSION_CRAWLER_NAMES {
            assert_ne!(
                FilteredTrafficReason::from_crawler_name(Some(name)),
                FilteredTrafficReason::Bot,
                "{name}"
            );
        }
        for kind in [
            TrafficFilterKind::Referrer,
            TrafficFilterKind::Ip,
            TrafficFilterKind::Asn,
            TrafficFilterKind::Country,
        ] {
            assert!(
                EXCLUSION_CRAWLER_NAMES.contains(&kind.crawler_name()),
                "{kind}"
            );
        }
        assert!(EXCLUSION_CRAWLER_NAMES.contains(&DATACENTER_CRAWLER_NAME));
        assert!(EXCLUSION_CRAWLER_NAMES.contains(&SPAM_REFERRER_CRAWLER_NAME));
        assert!(!EXCLUSION_CRAWLER_NAMES.contains(&"claudebot"));
    }

    #[test]
    fn canonical_referrer_host_lowercases_and_drops_the_root_dot() {
        assert_eq!(
            canonical_referrer_host("Spam-Site.Example."),
            "spam-site.example"
        );
        assert!(matches!(
            canonical_referrer_host("spam-site.example"),
            Cow::Borrowed("spam-site.example")
        ));
    }

    #[test]
    fn country_keys_fold_case_and_reject_anything_but_two_letters() {
        assert_eq!(country_key("sg"), Some(*b"SG"));
        assert_eq!(country_key("SG"), Some(*b"SG"));
        assert_eq!(country_key("SGP"), None);
        assert_eq!(country_key("S1"), None);
        assert_eq!(country_key(""), None);
    }

    #[test]
    fn no_rules_and_no_attributes_never_match() {
        assert!(compiled(&[]).is_empty());
        assert_eq!(
            compiled(&[]).matching_kind(&TrafficAttributes {
                referrer_host: Some("spam-site.example"),
                ip: Some(ip("203.0.113.1")),
                asn_number: Some(1),
                country_code: Some("SG"),
            }),
            None
        );
        let rules = compiled(&[("referrer", "spam-site.example"), ("asn", "1")]);
        assert_eq!(rules.matching_kind(&TrafficAttributes::default()), None);
    }

    #[test]
    fn unreadable_stored_rules_are_skipped_not_fatal() {
        let rules = compiled(&[
            ("ip", "garbage"),
            ("asn", "x"),
            ("bogus", "value"),
            ("asn", "7"),
        ]);
        assert_eq!(
            rules.matching_kind(&TrafficAttributes {
                asn_number: Some(7),
                ..Default::default()
            }),
            Some(TrafficFilterKind::Asn)
        );
    }

    // --- service -------------------------------------------------------

    fn project_id_row(id: i32) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([("id", Value::Int(Some(id)))])
    }

    fn count_row(n: i64) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([("num_items", Value::BigInt(Some(n)))])
    }

    fn rule_row(id: i32, kind: &str, value: &str) -> analytics_traffic_filters::Model {
        analytics_traffic_filters::Model {
            id,
            project_id: 1,
            kind: kind.to_string(),
            value: value.to_string(),
            note: None,
            created_by: Some(1),
            created_at: Utc::now(),
        }
    }

    fn kind_value_row(kind: &str, value: &str) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([
            ("kind", Value::String(Some(Box::new(kind.to_string())))),
            ("value", Value::String(Some(Box::new(value.to_string())))),
        ])
    }

    fn service(db: MockDatabase) -> TrafficFilterService {
        TrafficFilterService::new(Arc::new(db.into_connection()))
    }

    fn new_filter(kind: TrafficFilterKind, value: &str) -> NewTrafficFilter {
        NewTrafficFilter {
            kind,
            value: value.to_string(),
            note: None,
        }
    }

    #[tokio::test]
    async fn create_normalises_and_saves_the_rule() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                // The advisory lock serializing creates for the project.
                .append_exec_results([lock_acquired()])
                .append_query_results([vec![project_id_row(1)]])
                .append_query_results([Vec::<analytics_traffic_filters::Model>::new()])
                .append_query_results([vec![count_row(0)]])
                .append_query_results([vec![rule_row(5, "referrer", "spam-site.example")]]),
        );
        let saved = svc
            .create(
                1,
                new_filter(
                    TrafficFilterKind::Referrer,
                    "https://www.spam-site.example/",
                ),
                Some(1),
            )
            .await
            .expect("rule saved");
        assert_eq!(saved.id, 5);
        assert_eq!(saved.kind, TrafficFilterKind::Referrer);
        assert_eq!(saved.value, "spam-site.example");
    }

    #[tokio::test]
    async fn create_validates_before_touching_the_database() {
        // No query results queued: any database call would fail the test
        // with a mock error instead of the validation error asserted here.
        let svc = service(MockDatabase::new(DatabaseBackend::Postgres));
        let err = svc
            .create(1, new_filter(TrafficFilterKind::Ip, "0.0.0.0/0"), None)
            .await
            .expect_err("too broad");
        assert!(
            matches!(err, TrafficFilterError::InvalidValue { .. }),
            "{err}"
        );

        let err = svc
            .create(
                1,
                NewTrafficFilter {
                    kind: TrafficFilterKind::Asn,
                    value: "1".to_string(),
                    note: Some("x".repeat(MAX_NOTE_LEN + 1)),
                },
                None,
            )
            .await
            .expect_err("note too long");
        assert!(
            matches!(err, TrafficFilterError::Validation { .. }),
            "{err}"
        );
    }

    /// Two concurrent creates of different values must not both pass the
    /// per-kind limit: the count and the insert run in one transaction that
    /// holds the project's advisory lock from before the count.
    #[tokio::test]
    async fn create_counts_and_inserts_under_the_project_lock() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([lock_acquired()])
                .append_query_results([vec![project_id_row(1)]])
                .append_query_results([Vec::<analytics_traffic_filters::Model>::new()])
                .append_query_results([vec![count_row(0)]])
                .append_query_results([vec![rule_row(5, "ip", "198.18.0.0/24")]])
                .into_connection(),
        );
        let svc = TrafficFilterService::new(db.clone());
        svc.create(
            1,
            NewTrafficFilter {
                kind: TrafficFilterKind::Ip,
                value: "198.18.0.0/24".into(),
                note: None,
            },
            None,
        )
        .await
        .expect("create succeeds");
        drop(svc);

        let Ok(db) = Arc::try_unwrap(db) else {
            panic!("the service was dropped, so this is the last handle");
        };
        let log = db.into_transaction_log();
        assert_eq!(
            log.len(),
            1,
            "one transaction for the whole create: {log:?}"
        );
        let sql = format!("{log:?}");
        let lock = sql.find("pg_advisory_xact_lock").expect("lock taken");
        let count = sql.find("COUNT").expect("count issued");
        let insert = sql.find("INSERT").expect("insert issued");
        assert!(lock < count && count < insert, "{sql}");
    }

    #[tokio::test]
    async fn create_in_an_unknown_project_is_not_found() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                // The advisory lock serializing creates for the project.
                .append_exec_results([lock_acquired()])
                .append_query_results([Vec::<BTreeMap<&str, Value>>::new()]),
        );
        let err = svc
            .create(404, new_filter(TrafficFilterKind::Asn, "1"), None)
            .await
            .expect_err("unknown project");
        assert!(matches!(
            err,
            TrafficFilterError::ProjectNotFound { project_id: 404 }
        ));
    }

    #[tokio::test]
    async fn create_past_the_per_kind_limit_is_rejected() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                // The advisory lock serializing creates for the project.
                .append_exec_results([lock_acquired()])
                .append_query_results([vec![project_id_row(1)]])
                .append_query_results([Vec::<analytics_traffic_filters::Model>::new()])
                .append_query_results([vec![count_row(MAX_FILTERS_PER_KIND as i64)]]),
        );
        let err = svc
            .create(1, new_filter(TrafficFilterKind::Asn, "1"), None)
            .await
            .expect_err("limit reached");
        assert!(matches!(
            err,
            TrafficFilterError::LimitReached {
                project_id: 1,
                kind: TrafficFilterKind::Asn,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_duplicate_rule_is_reported_as_such() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                // The advisory lock serializing creates for the project.
                .append_exec_results([lock_acquired()])
                .append_query_results([vec![project_id_row(1)]])
                .append_query_results([vec![rule_row(4, "country", "SG")]]),
        );
        let err = svc
            .create(1, new_filter(TrafficFilterKind::Country, "sg"), None)
            .await
            .expect_err("duplicate");
        assert!(
            matches!(err, TrafficFilterError::Duplicate { ref value, .. } if value == "SG"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn delete_of_a_missing_rule_is_not_found_with_both_ids() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<analytics_traffic_filters::Model>::new()]),
        );
        let err = svc.delete(1, 99).await.expect_err("missing rule");
        assert!(matches!(
            err,
            TrafficFilterError::FilterNotFound {
                filter_id: 99,
                project_id: 1
            }
        ));
        assert!(err.to_string().contains("99") && err.to_string().contains("project 1"));
    }

    #[tokio::test]
    async fn compiled_rules_are_cached_and_a_write_invalidates_them() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                // first ingest read
                .append_query_results([vec![kind_value_row("asn", "7")]])
                // delete(): find, then delete
                .append_query_results([vec![rule_row(3, "asn", "7")]])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                // next ingest read after invalidation
                .append_query_results([Vec::<BTreeMap<&str, Value>>::new()]),
        );
        let traffic = TrafficAttributes {
            asn_number: Some(7),
            ..Default::default()
        };
        assert_eq!(
            svc.compiled(1).await.matching_kind(&traffic),
            Some(TrafficFilterKind::Asn)
        );
        // Served from the cache: no query result is queued for this read
        // ahead of the delete's own queries.
        assert_eq!(
            svc.compiled(1).await.matching_kind(&traffic),
            Some(TrafficFilterKind::Asn)
        );

        svc.delete(1, 3).await.expect("rule deleted");
        assert_eq!(svc.compiled(1).await.matching_kind(&traffic), None);
    }

    #[tokio::test]
    async fn a_database_error_on_the_ingest_read_matches_nothing() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("connection reset".to_string())]),
        );
        assert!(svc.compiled(1).await.is_empty());
    }

    #[tokio::test]
    async fn filtered_traffic_rejects_an_out_of_range_window() {
        let svc = service(MockDatabase::new(DatabaseBackend::Postgres));
        for days in [0, MAX_FILTERED_TRAFFIC_DAYS + 1] {
            let err = svc.filtered_traffic(1, days).await.expect_err("bad window");
            assert!(matches!(err, TrafficFilterError::Validation { .. }));
        }
    }

    #[tokio::test]
    async fn filtered_traffic_groups_flagged_events_by_reason() {
        let row = |is_crawler: bool, name: Option<&str>, events: i64| {
            BTreeMap::from([
                ("is_crawler", Value::Bool(Some(is_crawler))),
                (
                    "crawler_name",
                    Value::String(name.map(|n| Box::new(n.to_string()))),
                ),
                ("events", Value::BigInt(Some(events))),
            ])
        };
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_id_row(1)]])
                .append_query_results([vec![
                    row(false, None, 900),
                    row(true, Some("datacenter"), 120),
                    row(true, Some("Googlebot"), 30),
                    row(true, Some("bingbot"), 12),
                    row(true, Some("rule:referrer"), 60),
                ]]),
        );
        let summary = svc.filtered_traffic(1, 7).await.expect("summary");
        assert_eq!(summary.counted_events, 900);
        assert_eq!(
            summary.filtered,
            vec![
                FilteredTrafficCount {
                    reason: FilteredTrafficReason::Datacenter,
                    events: 120
                },
                FilteredTrafficCount {
                    reason: FilteredTrafficReason::ReferrerRule,
                    events: 60
                },
                FilteredTrafficCount {
                    reason: FilteredTrafficReason::Bot,
                    events: 42
                },
            ]
        );
    }
}
