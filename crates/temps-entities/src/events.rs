// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `SeaORM` Entity for events table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Every `crawler_name` a project traffic filter stores on an event it
/// excludes (as opposed to the bot name a user-agent match stores). Shared
/// here because both the ingest path that writes these values and every
/// report that leaves them out depend on this crate.
pub const TRAFFIC_FILTER_CRAWLER_NAMES: [&str; 6] = [
    "datacenter",
    "spam-referrer",
    "rule:referrer",
    "rule:ip",
    "rule:asn",
    "rule:country",
];

/// PostgreSQL predicate, true for an event a traffic filter did **not**
/// exclude. `alias` is the events table alias (`"e"`), or `""` for an
/// unqualified query; it is a compile-time constant, never user input.
///
/// Reports that predate traffic filters and never left bot rows out use
/// this rather than `is_crawler = false`: events sent without a user agent
/// (server-side ingestion through the API) are stored as the bot
/// `unknown`, and must keep counting.
pub fn not_excluded_by_traffic_filter(alias: &str) -> String {
    let column = |name: &str| {
        if alias.is_empty() {
            name.to_string()
        } else {
            format!("{alias}.{name}")
        }
    };
    format!(
        "NOT ({} AND COALESCE({}, '') IN ({}))",
        column("is_crawler"),
        column("crawler_name"),
        quoted_traffic_filter_names()
    )
}

/// [`TRAFFIC_FILTER_CRAWLER_NAMES`] as a SQL list (`'datacenter', ...`), for
/// both PostgreSQL and ClickHouse. The names are fixed literals without
/// quotes, so the list needs no escaping.
pub fn quoted_traffic_filter_names() -> String {
    TRAFFIC_FILTER_CRAWLER_NAMES
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub timestamp: DBDateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,

    // Session tracking
    pub session_id: Option<String>,
    pub visitor_id: Option<i32>,

    // Page data
    pub hostname: String,
    pub pathname: String,
    pub page_path: String, // For analytics grouping
    pub href: String,
    pub querystring: Option<String>,
    pub page_title: Option<String>,
    pub referrer: Option<String>,
    pub referrer_hostname: Option<String>,

    // Session flow tracking
    pub is_entry: bool,
    pub is_exit: bool,
    pub is_bounce: bool,
    pub time_on_page: Option<i32>,
    pub session_page_number: Option<i32>,

    // User interaction metrics
    pub scroll_depth: Option<i32>,
    pub clicks: Option<i32>,
    pub custom_properties: Option<serde_json::Value>,

    // Performance metrics
    pub lcp: Option<f32>,
    pub cls: Option<f32>,
    pub inp: Option<f32>,
    pub fcp: Option<f32>,
    pub ttfb: Option<f32>,
    pub fid: Option<f32>,

    // Device/Browser
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    pub screen_width: Option<i16>,
    pub screen_height: Option<i16>,
    pub viewport_width: Option<i16>,
    pub viewport_height: Option<i16>,

    // Geography (cached)
    pub ip_geolocation_id: Option<i32>,

    // Traffic source
    pub channel: Option<String>,
    pub utm_source: Option<String>,
    pub utm_medium: Option<String>,
    pub utm_campaign: Option<String>,
    pub utm_term: Option<String>,
    pub utm_content: Option<String>,

    // Event details
    pub event_type: String,
    pub event_name: Option<String>,
    pub props: Option<serde_json::Value>,

    // Analytics compatibility fields
    pub event_data: Option<String>,
    pub request_query: Option<String>,

    // Metadata
    pub user_agent: Option<String>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub language: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Projects,
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::DeploymentId",
        to = "super::deployments::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Deployments,
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Environments,
    #[sea_orm(
        belongs_to = "super::visitor::Entity",
        from = "Column::VisitorId",
        to = "super::visitor::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Visitor,
    #[sea_orm(
        belongs_to = "super::ip_geolocations::Entity",
        from = "Column::IpGeolocationId",
        to = "super::ip_geolocations::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    IpGeolocations,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deployments.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environments.def()
    }
}

impl Related<super::visitor::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Visitor.def()
    }
}

impl Related<super::ip_geolocations::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::IpGeolocations.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_predicate_keeps_bots_and_server_side_events_and_drops_filtered_ones() {
        assert_eq!(
            not_excluded_by_traffic_filter("e"),
            "NOT (e.is_crawler AND COALESCE(e.crawler_name, '') IN ('datacenter', \
             'spam-referrer', 'rule:referrer', 'rule:ip', 'rule:asn', 'rule:country'))"
        );
        assert!(not_excluded_by_traffic_filter("").starts_with("NOT (is_crawler AND"));
    }

    #[test]
    fn filter_names_need_no_sql_escaping() {
        for name in TRAFFIC_FILTER_CRAWLER_NAMES {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '-'),
                "{name}"
            );
        }
    }
}
