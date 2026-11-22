// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCronMonitor {
    pub slug: String,
    pub environment_id: Option<i32>,
    pub config: MonitorConfig,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateCronMonitor {
    pub config: Option<MonitorConfig>,
    pub enabled: Option<bool>,
}
#[derive(Serialize, ToSchema)]
pub struct CronMonitorView {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub slug: String,
    pub config: Option<MonitorConfig>,
    pub enabled: bool,
    /// Editing the configuration in Temps makes it authoritative over SDK config.
    pub configuration_source: String,
    pub status: String,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_check_in_at: Option<DateTime<Utc>>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub next_expected_at: Option<DateTime<Utc>>,
    pub alarm_id: Option<i32>,
}
impl TryFrom<CronMonitor> for CronMonitorView {
    type Error = CronMonitorError;
    fn try_from(m: CronMonitor) -> Result<Self, Self::Error> {
        let config = if m.config.is_null() {
            None
        } else {
            Some(
                serde_json::from_value(m.config)
                    .map_err(|_| CronMonitorError::StoredConfiguration { monitor_id: m.id })?,
            )
        };
        Ok(Self {
            id: m.id,
            project_id: m.project_id,
            environment_id: m.environment_id,
            slug: m.slug,
            next_expected_at: if m.enabled && config.is_some() {
                Some(m.next_expected_at)
            } else {
                None
            },
            config,
            enabled: m.enabled,
            configuration_source: if m.managed_config { "temps" } else { "sdk" }.into(),
            status: if m.enabled { m.status } else { "paused".into() },
            consecutive_failures: m.consecutive_failures,
            consecutive_successes: m.consecutive_successes,
            last_check_in_at: m.last_check_in_at,
            alarm_id: m.alarm_id,
        })
    }
}
#[derive(Serialize, ToSchema)]
pub struct CronMonitorList {
    pub items: Vec<CronMonitorView>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}
#[derive(Serialize, ToSchema)]
pub struct CronMonitorRunList {
    pub items: Vec<CronMonitorRun>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}
fn pagination(page: u64, size: u64) -> Result<(i64, i64), CronMonitorError> {
    if page == 0 || !(1..=100).contains(&size) {
        return Err(CronMonitorError::Invalid {
            reason: "page must be positive; page_size must be 1–100".into(),
        });
    }
    let offset = page
        .checked_sub(1)
        .and_then(|v| v.checked_mul(size))
        .and_then(|v| i64::try_from(v).ok())
        .ok_or_else(|| CronMonitorError::Invalid {
            reason: "page is too large".into(),
        })?;
    Ok((size as i64, offset))
}
impl CronMonitorService {
    async fn management_scope<C: ConnectionTrait>(
        db: &C,
        project: i32,
        environment: Option<i32>,
    ) -> Result<(), CronMonitorError> {
        if db.query_one(sql("SELECT id FROM projects WHERE id=$1 AND NOT is_deleted AND deleted_at IS NULL AND ($2::int IS NULL OR EXISTS(SELECT 1 FROM environments WHERE id=$2 AND project_id=$1 AND deleted_at IS NULL))",vec![project.into(),environment.into()])).await?.is_none() {
            return Err(CronMonitorError::ScopeNotFound {project_id:project, environment_id:environment});
        }
        Ok(())
    }
    async fn management_lock(&self, project: i32) -> Result<DatabaseTransaction, CronMonitorError> {
        let tx = self.db.begin().await?;
        tx.execute(sql(
            "SELECT pg_advisory_xact_lock(1936027246,$1)",
            vec![project.into()],
        ))
        .await?;
        Self::management_scope(&tx, project, None).await?;
        Ok(tx)
    }
    async fn locked_monitor(
        tx: &DatabaseTransaction,
        project: i32,
        id: i32,
    ) -> Result<CronMonitor, CronMonitorError> {
        CronMonitor::find_by_statement(sql(
            "SELECT * FROM cron_monitors WHERE project_id=$1 AND id=$2 FOR UPDATE",
            vec![project.into(), id.into()],
        ))
        .one(tx)
        .await?
        .ok_or(CronMonitorError::NotFound {
            project_id: project,
            monitor_id: id,
        })
    }
    pub async fn get_monitor(
        &self,
        project: i32,
        id: i32,
    ) -> Result<CronMonitorView, CronMonitorError> {
        async {
            Self::management_scope(self.db.as_ref(), project, None).await?;
            CronMonitor::find_by_statement(sql(
                "SELECT * FROM cron_monitors WHERE project_id=$1 AND id=$2",
                vec![project.into(), id.into()],
            ))
            .one(self.db.as_ref())
            .await?
            .ok_or(CronMonitorError::NotFound {
                project_id: project,
                monitor_id: id,
            })?
            .try_into()
        }
        .await
        .map_err(|error: CronMonitorError| {
            error.with_context("get_monitor", Some(project), Some(id))
        })
    }
    pub async fn list_monitors(
        &self,
        project: i32,
        page: u64,
        size: u64,
    ) -> Result<CronMonitorList, CronMonitorError> {
        async {
            let (limit, offset) = pagination(page, size)?;
            Self::management_scope(self.db.as_ref(), project, None).await?;
            let total = self
                .db
                .query_one(sql(
                    "SELECT count(*) AS n FROM cron_monitors WHERE project_id=$1",
                    vec![project.into()],
                ))
                .await?
                .ok_or_else(|| DbErr::Custom("monitor count missing".into()))?
                .try_get::<i64>("", "n")? as u64;
            let items = CronMonitor::find_by_statement(sql(
            "SELECT * FROM cron_monitors WHERE project_id=$1 ORDER BY id DESC LIMIT $2 OFFSET $3",
            vec![project.into(), limit.into(), offset.into()],
        ))
        .all(self.db.as_ref())
        .await?
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()?;
            Ok(CronMonitorList {
                items,
                total,
                page,
                page_size: size,
            })
        }
        .await
        .map_err(|error: CronMonitorError| error.with_context("list_monitors", Some(project), None))
    }
    pub async fn list_monitor_runs(
        &self,
        project: i32,
        id: i32,
        page: u64,
        size: u64,
    ) -> Result<CronMonitorRunList, CronMonitorError> {
        async {
            let (limit, offset) = pagination(page, size)?;
            self.get_monitor(project, id).await?;
            let total = self
                .db
                .query_one(sql(
                    "SELECT count(*) AS n FROM cron_monitor_runs WHERE monitor_id=$1",
                    vec![id.into()],
                ))
                .await?
                .ok_or_else(|| DbErr::Custom("run count missing".into()))?
                .try_get::<i64>("", "n")? as u64;
            let items = self.runs(project, id, limit as u64, offset as u64).await?;
            Ok(CronMonitorRunList {
                items,
                total,
                page,
                page_size: size,
            })
        }
        .await
        .map_err(|error: CronMonitorError| {
            error.with_context("list_monitor_runs", Some(project), Some(id))
        })
    }
    pub async fn create_monitor(
        &self,
        project: i32,
        input: CreateCronMonitor,
    ) -> Result<CronMonitorView, CronMonitorError> {
        async {
        let now = Utc::now();
        input.config.validate(now)?;
        if input.slug.is_empty()
            || input.slug.len() > 128
            || !input
                .slug
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(CronMonitorError::Invalid { reason: "slug must contain 1–128 letters, digits, underscores or hyphens".into() });
        }
        let tx = self.management_lock(project).await?;
        Self::management_scope(&tx, project, input.environment_id).await?;
        if tx.query_one(sql("SELECT id FROM cron_monitors WHERE project_id=$1 AND environment_id IS NOT DISTINCT FROM $2 AND slug=$3",vec![project.into(),input.environment_id.into(),input.slug.clone().into()])).await?.is_some() {return Err(CronMonitorError::Conflict);}
        let n = tx
            .query_one(sql(
                "SELECT count(*) AS n FROM cron_monitors WHERE project_id=$1",
                vec![project.into()],
            ))
            .await?
            .ok_or_else(|| DbErr::Custom("monitor count missing".into()))?
            .try_get::<i64>("", "n")?;
        if n >= 100 {
            return Err(CronMonitorError::Limit);
        }
        let next = input.config.next_after(now)?;
        let due = next + Duration::minutes(i64::from(input.config.checkin_margin));
        let config = serde_json::to_value(&input.config)
            .map_err(|_| CronMonitorError::Invalid { reason: "configuration could not be encoded".into() })?;
        let result=CronMonitor::find_by_statement(sql("INSERT INTO cron_monitors(project_id,environment_id,slug,config,next_expected_at,next_check_at,managed_config) VALUES($1,$2,$3,$4,$5,$6,true) RETURNING *",vec![project.into(),input.environment_id.into(),input.slug.into(),config.into(),next.into(),due.into()])).one(&tx).await?.ok_or_else(||DbErr::Custom("monitor missing after insert".into()))?;
        tx.commit().await?;
        result.try_into()

        }.await.map_err(|error: CronMonitorError| error.with_context("create_monitor", Some(project), None))
    }
    /// Pause keeps accepting history, but suspends health evaluation. Resume
    /// starts a fresh schedule and grants existing runs a fresh runtime budget.
    pub async fn update_monitor(
        &self,
        project: i32,
        id: i32,
        input: UpdateCronMonitor,
    ) -> Result<CronMonitorView, CronMonitorError> {
        async {
        let now = Utc::now();
        if input.config.is_none() && input.enabled.is_none() {
            return Err(CronMonitorError::Invalid { reason: "supply config or enabled".into() });
        }
        if let Some(config) = &input.config {
            config.validate(now)?;
        }
        let tx = self.management_lock(project).await?;
        let monitor = Self::locked_monitor(&tx, project, id).await?;
        let old_enabled = monitor.enabled;
        let enabled = input.enabled.unwrap_or(old_enabled);
        let changed_config = input.config.is_some();
        let config: Option<MonitorConfig> = match input.config {
            Some(c) => Some(c),
            None if monitor.config.is_null() => None,
            None => Some(
                serde_json::from_value(monitor.config.clone())
                    .map_err(|_| CronMonitorError::StoredConfiguration { monitor_id: id })?,
            ),
        };
        if enabled && config.is_none() {
            return Err(CronMonitorError::Invalid { reason: "configure a schedule before resuming".into() });
        }
        let reset = changed_config || (!old_enabled && enabled);
        let next = if reset {
            config
                .as_ref()
                .ok_or(CronMonitorError::StoredConfiguration { monitor_id: id })?
                .next_after(now)?
        } else {
            monitor.next_expected_at
        };
        let json = serde_json::to_value(&config)
            .map_err(|_| CronMonitorError::Invalid { reason: "configuration could not be encoded".into() })?;
        tx.execute(sql("UPDATE cron_monitors SET enabled=$2,config=$3,managed_config=managed_config OR $4,next_expected_at=$5,status=CASE WHEN $6 THEN 'unknown' ELSE status END,consecutive_failures=CASE WHEN $6 THEN 0 ELSE consecutive_failures END,consecutive_successes=CASE WHEN $6 THEN 0 ELSE consecutive_successes END,alarm_pending=alarm_pending OR $6 OR NOT $2 WHERE id=$1",vec![id.into(),enabled.into(),json.into(),changed_config.into(),next.into(),reset.into()])).await?;
        if reset {
            if let Some(config) = &config {
                tx.execute(sql("UPDATE cron_monitor_runs SET deadline_at=CASE WHEN $3 THEN $4 ELSE started_at END + $2::int * INTERVAL '1 minute' WHERE monitor_id=$1 AND status='in_progress'",vec![id.into(),(config.max_runtime as i32).into(),(!old_enabled && enabled).into(),now.into()])).await?;
            }
        }
        if let Some(config) = &config {
            Self::reschedule(&tx, id, config).await?;
        }
        let result = Self::locked_monitor(&tx, project, id).await?;
        tx.commit().await?;
        result.try_into()

        }.await.map_err(|error: CronMonitorError| error.with_context("update_monitor", Some(project), Some(id)))
    }
    pub async fn delete_monitor(
        &self,
        project: i32,
        id: i32,
        alarms: &crate::AlarmService,
    ) -> Result<(), CronMonitorError> {
        async {
            let tx = self.management_lock(project).await?;
            let monitor = Self::locked_monitor(&tx, project, id).await?;
            if let Some(alarm) = monitor.alarm_id {
                alarms.resolve_alarm(alarm, Some(project)).await?;
            }
            tx.execute(sql(
                "DELETE FROM cron_monitors WHERE id=$1",
                vec![id.into()],
            ))
            .await?;
            tx.commit().await?;
            Ok(())
        }
        .await
        .map_err(|error: CronMonitorError| {
            error.with_context("delete_monitor", Some(project), Some(id))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    #[tokio::test]
    async fn management_owns_configuration_pauses_and_detects_never_started_jobs() {
        let db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) if temps_test_utils::is_container_runtime_unavailable(&e.to_string()) => {
                eprintln!("Cron management DB integration unavailable: {e}");
                return;
            }
            Err(e) => panic!("Cron management DB setup failed: {e}"),
        };
        let project = temps_entities::projects::ActiveModel {
            name: Set("Synthetic monitor management".into()),
            slug: Set(uuid::Uuid::new_v4().to_string()),
            repo_name: Set("synthetic".into()),
            repo_owner: Set("synthetic".into()),
            directory: Set("/synthetic".into()),
            main_branch: Set("main".into()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.connection())
        .await
        .unwrap();
        let service = CronMonitorService::new(db.connection_arc());
        let cfg:MonitorConfig=serde_json::from_value(serde_json::json!({"schedule":{"type":"interval","value":5,"unit":"minute"},"max_runtime":1})).unwrap();
        let create = || CreateCronMonitor {
            slug: "synthetic-job".into(),
            environment_id: None,
            config: cfg.clone(),
        };
        let monitor = service.create_monitor(project.id, create()).await.unwrap();
        assert!(monitor.last_check_in_at.is_none());
        assert_eq!(monitor.configuration_source, "temps");
        assert!(matches!(
            service.create_monitor(project.id, create()).await,
            Err(CronMonitorError::Conflict)
        ));
        assert!(matches!(
            service
                .create_monitor(
                    project.id,
                    CreateCronMonitor {
                        environment_id: Some(i32::MAX),
                        ..create()
                    }
                )
                .await,
            Err(CronMonitorError::ScopeNotFound { .. })
        ));
        assert!(matches!(
            service.get_monitor(i32::MAX, monitor.id).await,
            Err(CronMonitorError::ScopeNotFound { .. })
        ));
        assert!(service
            .list_monitor_runs(project.id, monitor.id, 0, 20)
            .await
            .is_err());
        assert!(service
            .list_monitors(project.id, u64::MAX, 100)
            .await
            .is_err());
        assert_eq!(
            service
                .list_monitors(project.id, 1, 20)
                .await
                .unwrap()
                .total,
            1
        );
        let first_due = monitor.next_expected_at.unwrap() + Duration::minutes(1);
        service.detect_due(first_due).await.unwrap();
        assert_eq!(
            service
                .get_monitor(project.id, monitor.id)
                .await
                .unwrap()
                .status,
            "failing"
        );
        assert_eq!(
            service
                .list_monitor_runs(project.id, monitor.id, 1, 20)
                .await
                .unwrap()
                .items[0]
                .status,
            "missed"
        );
        let alarms = crate::AlarmService::new(
            db.connection_arc(),
            Arc::new(super::super::tests::Notifications),
            Arc::new(super::super::tests::Queue),
        );
        service.sync_alarms(&alarms).await.unwrap();
        assert!(service
            .get_monitor(project.id, monitor.id)
            .await
            .unwrap()
            .alarm_id
            .is_some());
        let paused = service
            .update_monitor(
                project.id,
                monitor.id,
                UpdateCronMonitor {
                    config: None,
                    enabled: Some(false),
                },
            )
            .await
            .unwrap();
        assert_eq!(paused.status, "paused");
        assert!(paused.next_expected_at.is_none());
        service.sync_alarms(&alarms).await.unwrap();
        assert!(service
            .get_monitor(project.id, monitor.id)
            .await
            .unwrap()
            .alarm_id
            .is_none());
        let unconfigured = MonitorCheckIn {
            id: "synthetic-unconfigured".into(),
            slug: "synthetic-unconfigured-job".into(),
            status: "in_progress".into(),
            config: None,
            duration: None,
            release: None,
            environment: None,
            trace_id: None,
            span_id: None,
        };
        service
            .ingest_at(
                project.id,
                None,
                None,
                vec![unconfigured.clone()],
                first_due,
            )
            .await
            .unwrap();
        let unconfigured_id = service
            .list(project.id, 100, 0)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.slug == unconfigured.slug)
            .unwrap()
            .id;
        service
            .update_monitor(
                project.id,
                unconfigured_id,
                UpdateCronMonitor {
                    config: None,
                    enabled: Some(false),
                },
            )
            .await
            .unwrap();
        let mut unconfigured_error = unconfigured;
        unconfigured_error.status = "error".into();
        service
            .ingest_at(project.id, None, None, vec![unconfigured_error], first_due)
            .await
            .unwrap();
        assert_eq!(
            service
                .get_monitor(project.id, unconfigured_id)
                .await
                .unwrap()
                .consecutive_failures,
            0
        );
        service
            .delete_monitor(project.id, unconfigured_id, &alarms)
            .await
            .unwrap();
        let mut sdk_cfg = cfg.clone();
        sdk_cfg.max_runtime = 30;
        let start = MonitorCheckIn {
            id: "synthetic-run".into(),
            slug: "synthetic-job".into(),
            status: "in_progress".into(),
            config: Some(sdk_cfg),
            duration: None,
            release: None,
            environment: None,
            trace_id: None,
            span_id: None,
        };
        service
            .ingest_at(project.id, None, None, vec![start.clone()], first_due)
            .await
            .unwrap();
        service
            .detect_due(first_due + Duration::days(1))
            .await
            .unwrap();
        assert_eq!(
            service.runs(project.id, monitor.id, 100, 0).await.unwrap()[0].status,
            "in_progress"
        );
        let mut finish = start;
        finish.status = "ok".into();
        service
            .ingest_at(
                project.id,
                None,
                None,
                vec![finish],
                first_due + Duration::days(1),
            )
            .await
            .unwrap();
        let paused = service.get_monitor(project.id, monitor.id).await.unwrap();
        assert_eq!(
            paused.config.unwrap().max_runtime,
            1,
            "stale SDK config cannot override native edits"
        );
        assert_eq!(
            paused.consecutive_failures, 1,
            "paused history cannot change health counters"
        );
        let resumed = service
            .update_monitor(
                project.id,
                monitor.id,
                UpdateCronMonitor {
                    config: None,
                    enabled: Some(true),
                },
            )
            .await
            .unwrap();
        assert_eq!(resumed.status, "unknown");
        assert_eq!(resumed.consecutive_failures, 0);
        assert!(resumed.next_expected_at.is_some());
        let mut edited = cfg;
        edited.max_runtime = 2;
        let edited = service
            .update_monitor(
                project.id,
                monitor.id,
                UpdateCronMonitor {
                    config: Some(edited),
                    enabled: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(edited.config.unwrap().max_runtime, 2);
        service
            .delete_monitor(project.id, monitor.id, &alarms)
            .await
            .unwrap();
        assert!(matches!(
            service.get_monitor(project.id, monitor.id).await,
            Err(CronMonitorError::NotFound { .. })
        ));
        assert_eq!(
            service
                .list_monitors(project.id, 1, 20)
                .await
                .unwrap()
                .total,
            0
        );
        assert!(service
            .runs(project.id, monitor.id, 20, 0)
            .await
            .unwrap()
            .is_empty());
    }
}
