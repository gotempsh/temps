// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reading funnel definitions back: one funnel with its steps, and the
//! paginated list. Steps are loaded for a whole page in one query.

use std::collections::HashMap;

use sea_orm::{ColumnTrait, DbErr, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder};
use serde_json::Value;
use temps_core::PageQuery;
use temps_entities::{funnel_steps, funnels};

use super::service::{FunnelService, SmartFilter};

/// A funnel and its steps, in step order.
#[derive(Debug, Clone)]
pub struct FunnelDefinition {
    pub funnel: funnels::Model,
    pub steps: Vec<funnel_steps::Model>,
}

/// Column names `SmartFilter::to_condition` stores simple filters under, in
/// the order they are reported back.
const SIMPLE_FILTER_COLUMNS: [&str; 11] = [
    "pathname",
    "hostname",
    "utm_source",
    "utm_campaign",
    "utm_medium",
    "referrer_hostname",
    "channel",
    "device_type",
    "browser",
    "operating_system",
    "language",
];

impl SmartFilter {
    /// The filters a step was stored with (`CreateFunnelStep::serialize_filters`).
    /// Simple filters come first in a fixed column order, then custom-data
    /// filters in the order they were stored. A stored value that cannot be
    /// read is skipped and logged rather than failing the read.
    pub fn from_stored(step_id: i32, stored: Option<&str>) -> Vec<SmartFilter> {
        let Some(stored) = stored else {
            return Vec::new();
        };
        let map: serde_json::Map<String, Value> = match serde_json::from_str(stored) {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!(step_id, error = %e, "funnels: unreadable stored step filter");
                return Vec::new();
            }
        };
        let mut filters: Vec<SmartFilter> = SIMPLE_FILTER_COLUMNS
            .iter()
            .filter_map(|column| {
                let value = map.get(*column)?.as_str()?.to_string();
                Some(match *column {
                    "pathname" => SmartFilter::PagePath(value),
                    "hostname" => SmartFilter::Hostname(value),
                    "utm_source" => SmartFilter::UtmSource(value),
                    "utm_campaign" => SmartFilter::UtmCampaign(value),
                    "utm_medium" => SmartFilter::UtmMedium(value),
                    "referrer_hostname" => SmartFilter::ReferrerHostname(value),
                    "channel" => SmartFilter::Channel(value),
                    "device_type" => SmartFilter::DeviceType(value),
                    "browser" => SmartFilter::Browser(value),
                    "operating_system" => SmartFilter::OperatingSystem(value),
                    _ => SmartFilter::Language(value),
                })
            })
            .collect();
        if let Some(Value::Array(custom)) = map.get("_custom_data") {
            filters.extend(custom.iter().filter_map(|entry| {
                Some(SmartFilter::CustomData {
                    path: entry.get("path")?.as_str()?.to_string(),
                    value: entry.get("value")?.as_str()?.to_string(),
                })
            }));
        }
        filters
    }
}

fn funnel_not_found(project_id: i32, funnel_id: i32) -> DbErr {
    DbErr::RecordNotFound(format!(
        "funnel {funnel_id} not found in project {project_id}"
    ))
}

impl FunnelService {
    /// One active funnel of `project_id`, with its steps.
    pub async fn get_funnel(
        &self,
        project_id: i32,
        funnel_id: i32,
    ) -> Result<FunnelDefinition, DbErr> {
        let funnel = funnels::Entity::find_by_id(funnel_id)
            .filter(funnels::Column::ProjectId.eq(project_id))
            .filter(funnels::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| funnel_not_found(project_id, funnel_id))?;
        let steps = funnel_steps::Entity::find()
            .filter(funnel_steps::Column::FunnelId.eq(funnel_id))
            .order_by_asc(funnel_steps::Column::StepOrder)
            .all(self.db.as_ref())
            .await?;
        Ok(FunnelDefinition { funnel, steps })
    }

    /// One page of the active funnels of `project_id`, newest first, each
    /// with its steps, and the number of active funnels.
    pub async fn list_funnel_page(
        &self,
        project_id: i32,
        page: PageQuery,
    ) -> Result<(Vec<FunnelDefinition>, u64), DbErr> {
        let (_, page_size) = page.normalize();
        let paginator = funnels::Entity::find()
            .filter(funnels::Column::ProjectId.eq(project_id))
            .filter(funnels::Column::IsActive.eq(true))
            .order_by_desc(funnels::Column::CreatedAt)
            .order_by_desc(funnels::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let funnels = paginator.fetch_page(page.page_index()).await?;
        if funnels.is_empty() {
            return Ok((Vec::new(), total));
        }

        let mut steps_by_funnel: HashMap<i32, Vec<funnel_steps::Model>> = HashMap::new();
        for step in funnel_steps::Entity::find()
            .filter(funnel_steps::Column::FunnelId.is_in(funnels.iter().map(|f| f.id)))
            .order_by_asc(funnel_steps::Column::FunnelId)
            .order_by_asc(funnel_steps::Column::StepOrder)
            .all(self.db.as_ref())
            .await?
        {
            steps_by_funnel
                .entry(step.funnel_id)
                .or_default()
                .push(step);
        }
        let page = funnels
            .into_iter()
            .map(|funnel| FunnelDefinition {
                steps: steps_by_funnel.remove(&funnel.id).unwrap_or_default(),
                funnel,
            })
            .collect();
        Ok((page, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::CreateFunnelStep;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Arc;

    fn funnel(id: i32) -> funnels::Model {
        let now = chrono::Utc::now();
        funnels::Model {
            id,
            project_id: 1,
            name: format!("Funnel {id}"),
            description: None,
            is_active: true,
            created_at: now,
            updated_at: now,
        }
    }

    fn step(id: i32, funnel_id: i32, order: i32, filter: Option<&str>) -> funnel_steps::Model {
        funnel_steps::Model {
            id,
            funnel_id,
            step_order: order,
            event_name: format!("event_{order}"),
            event_filter: filter.map(str::to_string),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn stored_filters_read_back_as_written() {
        let written = CreateFunnelStep {
            event_name: "signup".to_string(),
            event_filter: vec![
                SmartFilter::UtmSource("newsletter".to_string()),
                SmartFilter::PagePath("/pricing".to_string()),
                SmartFilter::CustomData {
                    path: "user.plan".to_string(),
                    value: "pro".to_string(),
                },
            ],
        };
        let stored = written.serialize_filters();
        let read = SmartFilter::from_stored(1, stored.as_deref());
        let json = serde_json::to_value(&read).unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                {"type": "page_path", "value": "/pricing"},
                {"type": "utm_source", "value": "newsletter"},
                {"type": "custom_data", "value": {"path": "user.plan", "value": "pro"}}
            ])
        );
        assert!(SmartFilter::from_stored(1, None).is_empty());
        assert!(SmartFilter::from_stored(1, Some("not json")).is_empty());
    }

    #[tokio::test]
    async fn get_funnel_returns_its_steps_or_a_not_found_naming_the_ids() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![funnel(7)]])
            .append_query_results([vec![step(1, 7, 1, None), step(2, 7, 2, None)]])
            .append_query_results([Vec::<funnels::Model>::new()])
            .into_connection();
        let service = FunnelService::new(Arc::new(db));

        let found = service.get_funnel(1, 7).await.unwrap();
        assert_eq!(found.funnel.id, 7);
        assert_eq!(
            found.steps.iter().map(|s| s.step_order).collect::<Vec<_>>(),
            vec![1, 2]
        );

        let missing = service.get_funnel(1, 8).await.unwrap_err();
        assert_eq!(
            missing,
            DbErr::RecordNotFound("funnel 8 not found in project 1".to_string())
        );
    }

    #[tokio::test]
    async fn a_page_loads_the_steps_of_all_its_funnels_in_one_query() {
        let count = |n: i64| {
            std::collections::BTreeMap::from([(
                "num_items".to_string(),
                sea_orm::Value::BigInt(Some(n)),
            )])
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count(3)]])
            .append_query_results([vec![funnel(9), funnel(8)]])
            .append_query_results([vec![step(1, 8, 1, None), step(2, 9, 1, None)]])
            .into_connection();
        let service = FunnelService::new(Arc::new(db));

        let query = PageQuery {
            page: Some(1),
            page_size: Some(2),
        };
        let (page, total) = service.list_funnel_page(1, query).await.unwrap();
        assert_eq!(total, 3);
        assert_eq!(
            page.iter()
                .map(|d| (d.funnel.id, d.steps.len()))
                .collect::<Vec<_>>(),
            vec![(9, 1), (8, 1)]
        );
    }
}
