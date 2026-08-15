//! Backend-neutral API traffic aggregation contract.
//!
//! Clients choose an allowlisted set of dimensions, metrics, filters, and
//! ordering. Storage implementations compile those enums to static SQL
//! fragments; no client-provided identifier is ever interpolated into SQL.

use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use utoipa::ToSchema;

use crate::service::proxy_log_service::ProxyLogServiceError;

pub const MAX_TRAFFIC_DIMENSIONS: usize = 4;
pub const MAX_TRAFFIC_METRICS: usize = 14;
pub const MAX_TRAFFIC_FILTERS: usize = 12;
pub const MAX_TRAFFIC_PAGE_SIZE: u64 = 100;
pub const MAX_TRAFFIC_PAGE: u64 = 10_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrafficDimension {
    ClientIp,
    Method,
    Path,
    Host,
    StatusCode,
    StatusClass,
    EnvironmentId,
    DeploymentId,
    RequestSource,
    IsBot,
    Browser,
    OperatingSystem,
    DeviceType,
    CacheStatus,
}

impl TrafficDimension {
    pub fn api_name(self) -> &'static str {
        match self {
            Self::ClientIp => "client_ip",
            Self::Method => "method",
            Self::Path => "path",
            Self::Host => "host",
            Self::StatusCode => "status_code",
            Self::StatusClass => "status_class",
            Self::EnvironmentId => "environment_id",
            Self::DeploymentId => "deployment_id",
            Self::RequestSource => "request_source",
            Self::IsBot => "is_bot",
            Self::Browser => "browser",
            Self::OperatingSystem => "operating_system",
            Self::DeviceType => "device_type",
            Self::CacheStatus => "cache_status",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrafficMetric {
    Requests,
    Errors,
    ErrorRate,
    LatencyAvg,
    LatencyMin,
    LatencyMax,
    LatencyP50,
    LatencyP95,
    LatencyP99,
    UniqueIps,
    UniquePaths,
    BotRequests,
    RobotsTxtRequests,
    LastSeen,
}

impl TrafficMetric {
    pub fn api_name(self) -> &'static str {
        match self {
            Self::Requests => "requests",
            Self::Errors => "errors",
            Self::ErrorRate => "error_rate",
            Self::LatencyAvg => "latency_avg",
            Self::LatencyMin => "latency_min",
            Self::LatencyMax => "latency_max",
            Self::LatencyP50 => "latency_p50",
            Self::LatencyP95 => "latency_p95",
            Self::LatencyP99 => "latency_p99",
            Self::UniqueIps => "unique_ips",
            Self::UniquePaths => "unique_paths",
            Self::BotRequests => "bot_requests",
            Self::RobotsTxtRequests => "robots_txt_requests",
            Self::LastSeen => "last_seen",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrafficFilterOperator {
    Eq,
    NotEq,
    Contains,
    StartsWith,
    In,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
pub struct TrafficFilter {
    pub dimension: TrafficDimension,
    pub operator: TrafficFilterOperator,
    /// One value for scalar operators; one or more values for `in`.
    pub values: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrafficSortDirection {
    Asc,
    Desc,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "field")]
pub enum TrafficOrderField {
    Dimension(TrafficDimension),
    Metric(TrafficMetric),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct TrafficOrderBy {
    pub field: TrafficOrderField,
    pub direction: TrafficSortDirection,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
pub struct TrafficAggregationRequest {
    #[schema(value_type = String, format = "date-time")]
    pub start_time: UtcDateTime,
    #[schema(value_type = String, format = "date-time")]
    pub end_time: UtcDateTime,
    pub environment_id: Option<i32>,
    /// Zero to four grouping dimensions. Zero returns one overall rollup;
    /// multiple dimensions form a tuple.
    pub dimensions: Vec<TrafficDimension>,
    /// Aggregates to calculate. Unrequested response fields remain null.
    pub metrics: Vec<TrafficMetric>,
    #[serde(default)]
    pub filters: Vec<TrafficFilter>,
    /// Ordered sort keys. Multiple keys provide stable pagination when the
    /// primary metric ties. Every field must be requested/grouped.
    #[serde(default)]
    pub order_by: Vec<TrafficOrderBy>,
    /// Include Temps status-monitor requests. They are excluded by default.
    #[serde(default)]
    pub include_synthetic: bool,
    #[serde(default = "default_page")]
    pub page: u64,
    #[serde(default = "default_page_size")]
    pub page_size: u64,
}

fn default_page() -> u64 {
    1
}

fn default_page_size() -> u64 {
    20
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
pub struct TrafficDimensionValue {
    pub dimension: TrafficDimension,
    /// Dimensions are serialized as display-safe strings across both storage
    /// backends. Null database values remain null rather than becoming empty.
    pub value: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, ToSchema)]
pub struct TrafficMetricValues {
    pub requests: Option<i64>,
    pub errors: Option<i64>,
    pub error_rate: Option<f64>,
    pub latency_avg_ms: Option<f64>,
    pub latency_min_ms: Option<f64>,
    pub latency_max_ms: Option<f64>,
    pub latency_p50_ms: Option<f64>,
    pub latency_p95_ms: Option<f64>,
    pub latency_p99_ms: Option<f64>,
    pub unique_ips: Option<i64>,
    pub unique_paths: Option<i64>,
    pub bot_requests: Option<i64>,
    pub robots_txt_requests: Option<i64>,
    #[schema(value_type = Option<String>, format = "date-time")]
    pub last_seen: Option<UtcDateTime>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
pub struct TrafficAggregationRow {
    pub dimensions: Vec<TrafficDimensionValue>,
    pub metrics: TrafficMetricValues,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
pub struct TrafficAggregationResponse {
    pub rows: Vec<TrafficAggregationRow>,
    pub total_groups: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
    pub dimensions: Vec<TrafficDimension>,
    pub metrics: Vec<TrafficMetric>,
    pub synthetic_excluded: bool,
}

impl TrafficAggregationRequest {
    pub fn validate(&self) -> Result<(), ProxyLogServiceError> {
        if self.start_time >= self.end_time {
            return Err(invalid("start_time must be before end_time"));
        }
        if self.end_time - self.start_time > chrono::Duration::days(30) {
            return Err(invalid("traffic aggregation windows cannot exceed 30 days"));
        }
        let span = self.end_time - self.start_time;
        let has_high_cardinality_dimension = self.dimensions.iter().any(|dimension| {
            matches!(
                dimension,
                TrafficDimension::ClientIp | TrafficDimension::Path | TrafficDimension::Host
            )
        });
        let has_high_cardinality_metric = self.metrics.iter().any(|metric| {
            matches!(
                metric,
                TrafficMetric::UniqueIps | TrafficMetric::UniquePaths
            )
        });
        if (has_high_cardinality_dimension || has_high_cardinality_metric)
            && span > chrono::Duration::days(7)
        {
            return Err(invalid(
                "traffic aggregations using high-cardinality dimensions or unique counts cannot exceed 7 days",
            ));
        }
        if self.dimensions.len() >= 3 && span > chrono::Duration::days(1) {
            return Err(invalid(
                "traffic aggregations with three or more dimensions cannot exceed 24 hours",
            ));
        }
        if self.dimensions.len() > MAX_TRAFFIC_DIMENSIONS {
            return Err(invalid(format!(
                "dimensions cannot contain more than {MAX_TRAFFIC_DIMENSIONS} entries"
            )));
        }
        if self.metrics.is_empty() || self.metrics.len() > MAX_TRAFFIC_METRICS {
            return Err(invalid(format!(
                "metrics must contain between 1 and {MAX_TRAFFIC_METRICS} entries"
            )));
        }
        if self.filters.len() > MAX_TRAFFIC_FILTERS {
            return Err(invalid(format!(
                "filters cannot contain more than {MAX_TRAFFIC_FILTERS} entries"
            )));
        }
        if self.page == 0 {
            return Err(invalid("page must be at least 1"));
        }
        if self.page > MAX_TRAFFIC_PAGE {
            return Err(invalid(format!("page cannot exceed {MAX_TRAFFIC_PAGE}")));
        }
        if self.page_size == 0 || self.page_size > MAX_TRAFFIC_PAGE_SIZE {
            return Err(invalid(format!(
                "page_size must be between 1 and {MAX_TRAFFIC_PAGE_SIZE}"
            )));
        }
        ensure_unique(&self.dimensions, "dimensions")?;
        ensure_unique(&self.metrics, "metrics")?;

        for filter in &self.filters {
            validate_filter(filter)?;
        }

        if self.order_by.len() > MAX_TRAFFIC_DIMENSIONS {
            return Err(invalid(format!(
                "order_by cannot contain more than {MAX_TRAFFIC_DIMENSIONS} entries"
            )));
        }
        ensure_unique(
            &self
                .order_by
                .iter()
                .map(|order| order.field)
                .collect::<Vec<_>>(),
            "order_by fields",
        )?;
        for order in &self.order_by {
            match order.field {
                TrafficOrderField::Dimension(dimension)
                    if !self.dimensions.contains(&dimension) =>
                {
                    return Err(invalid(format!(
                        "order dimension '{}' must also be grouped",
                        dimension.api_name()
                    )));
                }
                TrafficOrderField::Metric(metric) if !self.metrics.contains(&metric) => {
                    return Err(invalid(format!(
                        "order metric '{}' must also be requested",
                        metric.api_name()
                    )));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn ensure_unique<T: Eq + std::hash::Hash + Copy>(
    values: &[T],
    label: &str,
) -> Result<(), ProxyLogServiceError> {
    let unique = values
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    if unique.len() != values.len() {
        return Err(invalid(format!("{label} cannot contain duplicates")));
    }
    Ok(())
}

fn validate_filter(filter: &TrafficFilter) -> Result<(), ProxyLogServiceError> {
    let expected = if filter.operator == TrafficFilterOperator::In {
        1..=20
    } else {
        1..=1
    };
    if !expected.contains(&filter.values.len()) {
        return Err(invalid(format!(
            "filter '{}' has an invalid number of values for {:?}",
            filter.dimension.api_name(),
            filter.operator
        )));
    }
    if filter.values.iter().any(|value| value.len() > 512) {
        return Err(invalid(format!(
            "filter '{}' values cannot exceed 512 characters",
            filter.dimension.api_name()
        )));
    }
    if matches!(
        filter.operator,
        TrafficFilterOperator::Contains | TrafficFilterOperator::StartsWith
    ) && !matches!(
        filter.dimension,
        TrafficDimension::Path
            | TrafficDimension::Host
            | TrafficDimension::ClientIp
            | TrafficDimension::Browser
            | TrafficDimension::OperatingSystem
            | TrafficDimension::DeviceType
            | TrafficDimension::CacheStatus
    ) {
        return Err(invalid(format!(
            "filter '{}' does not support {:?}",
            filter.dimension.api_name(),
            filter.operator
        )));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> ProxyLogServiceError {
    ProxyLogServiceError::InvalidFilter(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> TrafficAggregationRequest {
        TrafficAggregationRequest {
            start_time: chrono::Utc::now() - chrono::Duration::hours(1),
            end_time: chrono::Utc::now(),
            environment_id: None,
            dimensions: vec![TrafficDimension::ClientIp, TrafficDimension::Path],
            metrics: vec![TrafficMetric::Requests, TrafficMetric::LatencyMax],
            filters: Vec::new(),
            order_by: vec![TrafficOrderBy {
                field: TrafficOrderField::Metric(TrafficMetric::Requests),
                direction: TrafficSortDirection::Desc,
            }],
            include_synthetic: false,
            page: 1,
            page_size: 20,
        }
    }

    #[test]
    fn accepts_multiple_dimensions_and_client_selected_metrics() {
        assert!(request().validate().is_ok());
    }

    #[test]
    fn rejects_sort_metric_that_was_not_requested() {
        let mut request = request();
        request.order_by = vec![TrafficOrderBy {
            field: TrafficOrderField::Metric(TrafficMetric::ErrorRate),
            direction: TrafficSortDirection::Desc,
        }];
        assert!(matches!(
            request.validate(),
            Err(ProxyLogServiceError::InvalidFilter(message))
                if message.contains("must also be requested")
        ));
    }

    #[test]
    fn rejects_duplicate_dimensions_and_unsupported_filter_operator() {
        let mut duplicate = request();
        duplicate.dimensions = vec![TrafficDimension::Path, TrafficDimension::Path];
        assert!(duplicate.validate().is_err());

        let mut unsupported = request();
        unsupported.filters.push(TrafficFilter {
            dimension: TrafficDimension::StatusCode,
            operator: TrafficFilterOperator::Contains,
            values: vec!["5".to_string()],
        });
        assert!(unsupported.validate().is_err());
    }

    #[test]
    fn accepts_zero_dimensions_for_an_overall_rollup() {
        let mut request = request();
        request.dimensions.clear();
        assert!(request.validate().is_ok());
    }

    #[test]
    fn bounds_high_cardinality_windows_and_page_offsets() {
        let mut high_cardinality = request();
        high_cardinality.start_time = high_cardinality.end_time - chrono::Duration::days(8);
        assert!(matches!(
            high_cardinality.validate(),
            Err(ProxyLogServiceError::InvalidFilter(message))
                if message.contains("cannot exceed 7 days")
        ));

        let mut unique_rollup = request();
        unique_rollup.dimensions.clear();
        unique_rollup.metrics = vec![TrafficMetric::UniqueIps];
        unique_rollup.start_time = unique_rollup.end_time - chrono::Duration::days(30);
        assert!(matches!(
            unique_rollup.validate(),
            Err(ProxyLogServiceError::InvalidFilter(message))
                if message.contains("cannot exceed 7 days")
        ));

        let mut three_dimensions = request();
        three_dimensions.dimensions.push(TrafficDimension::Method);
        three_dimensions.start_time = three_dimensions.end_time - chrono::Duration::hours(25);
        assert!(matches!(
            three_dimensions.validate(),
            Err(ProxyLogServiceError::InvalidFilter(message))
                if message.contains("cannot exceed 24 hours")
        ));

        let mut distant_page = request();
        distant_page.page = MAX_TRAFFIC_PAGE + 1;
        assert!(distant_page.validate().is_err());
    }
}
