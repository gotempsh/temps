// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps backups` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  attachScheduleServices,
  cancelBackup,
  cancelScheduleRun,
  detachScheduleService,
  getExternalServiceBackupCapability,
  listBackupAlerts,
  listBackupChildren,
  listExternalServiceBackups,
  listScheduleRunJobs,
  listScheduleRuns,
  listScheduleServices,
  listServiceSchedules,
  runScheduleNow,
  setDefaultS3Source,
  testS3ConnectionPreview,
  testS3SourceConnection,
  updateBackupSchedule,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerBackupsEndpoints(program: Command): void {
  const group = topGroup(program, 'backups', "backups")

  endpoint(group, listBackupAlerts, {
    name: "list-alerts",
    description: "List open backup alerts",
  })

  endpoint(group, getExternalServiceBackupCapability, {
    name: "get-external-service-capability",
    description: "Report whether an existing service can produce a Cloud-restorable backup",
    path: { id: 'int' },
  })

  endpoint(group, listExternalServiceBackups, {
    name: "list-external-service",
    description: "List all backups for a specific external service (DB-only, no S3 scan)",
    path: { service_id: 'int' },
    query: {
      page: { kind: 'int', help: "Page number (1-based)" },
      page_size: { kind: 'int', help: "Items per page" },
    },
  })

  endpoint(group, listServiceSchedules, {
    name: "list-service-schedules",
    description: "List the schedules that target a specific external service",
    path: { service_id: 'int' },
  })

  endpoint(group, testS3ConnectionPreview, {
    name: "test-s3-connection-preview",
    description: "Test S3 connectivity against a prospective source configuration (before creating it)",
    body: {
      access_key_id: { kind: 'string!', help: "Access key ID of the bucket's credentials, for example 'AKIA...' on AWS" },
      backing_service_id: { kind: 'int', help: "Managed RustFS/S3 service that supplies this destination" },
      bucket_name: 'string!',
      bucket_path: 'string!',
      endpoint: { kind: 'string', help: "Optional endpoint for S3-compatible services like MinIO or RustFS, with or without a scheme" },
      force_path_style: { kind: 'bool', help: "Whether to use path-style addressing (default: true)" },
      is_default: { kind: 'bool', help: "When true, make this the default source (will swap out any existing default)" },
      name: 'string!',
      region: 'string!',
      secret_key: { kind: 'secret!', help: "Secret access key that goes with the access key ID" },
      use_tls: { kind: 'bool', help: "Whether the endpoint is reached over TLS" },
      worker_credentials: { kind: 'string', help: "What a worker node may receive from an S3 source (ADR-102)", values: ["scoped","allow_full"] },
    },
  })

  endpoint(group, setDefaultS3Source, {
    name: "set-default-s3-source",
    description: "Mark an S3 source as the default",
    path: { id: 'int' },
  })

  endpoint(group, testS3SourceConnection, {
    name: "test-s3-source-connection",
    description: "Test connectivity to an existing S3 source using its stored credentials",
    path: { id: 'int' },
  })

  endpoint(group, cancelScheduleRun, {
    name: "cancel-schedule-run",
    description: "Cancel every non-terminal child backup belonging to a schedule run",
    path: { id: 'int' },
    confirm: "This will cancel every non-terminal child backup belonging to a schedule run.",
  })

  endpoint(group, listScheduleRunJobs, {
    name: "list-schedule-run-jobs",
    description: "List the individual backup jobs for a single scheduler run",
    path: { id: 'int' },
  })

  endpoint(group, updateBackupSchedule, {
    name: "update-schedule",
    description: "Update a backup schedule (partial update)",
    path: { id: 'int' },
    body: {
      description: { kind: 'string', help: "New human-readable description" },
      enabled: { kind: 'bool', help: "Enable or disable the schedule" },
      include_control_plane: { kind: 'bool', help: "Toggle whether the control-plane backup is produced on every run" },
      max_runtime_secs: { kind: 'int', help: "Per-schedule wall-clock timeout override (seconds)" },
      name: { kind: 'string', help: "New schedule name" },
      retention_period: { kind: 'int', help: "Days to retain backups produced by this schedule" },
      schedule_expression: { kind: 'string', help: "New cron expression with a leading seconds field ('sec min hour day-of-month month day-of-week', e.g. '0 0..." },
      service_ids: { kind: 'ints', help: "Replace the explicit external-service selection atomically with the schedule update" },
      tags: { kind: 'strings', help: "Replace the full tag list" },
      target_all_services: { kind: 'bool', help: "Toggle between \"back up every database\" ('true') and \"back up only the explicit list\" ('false')" },
    },
  })

  endpoint(group, runScheduleNow, {
    name: "run-schedule-now",
    description: "Immediately fan-out a run for the given schedule (Run Now)",
    path: { id: 'int' },
  })

  endpoint(group, listScheduleRuns, {
    name: "list-schedule-runs",
    description: "Paginated run history for a backup schedule (one row per scheduler tick)",
    path: { id: 'int' },
    query: {
      page: { kind: 'int', help: "Page number (1-based, defaults to 1, clamped to 1 if < 1)" },
      page_size: { kind: 'int', help: "Items per page (defaults to 20, clamped to 100 if > 100)" },
    },
  })

  endpoint(group, listScheduleServices, {
    name: "list-schedule-services",
    description: "List the external services attached to a backup schedule",
    path: { id: 'int' },
  })

  endpoint(group, attachScheduleServices, {
    name: "attach-schedule-services",
    description: "Attach one or more external services to a backup schedule",
    path: { id: 'int' },
    body: {
      service_ids: { kind: 'ints!', help: "External service ids to attach" },
    },
  })

  endpoint(group, detachScheduleService, {
    name: "detach-schedule-service",
    description: "Detach a single external service from a backup schedule",
    path: { id: 'int', service_id: 'int' },
    confirm: "This will detach a single external service from a backup schedule.",
  })

  endpoint(group, cancelBackup, {
    name: "cancel",
    description: "Cancel a single in-flight backup",
    path: { id: 'int' },
    confirm: "This will cancel a single in-flight backup.",
  })

  endpoint(group, listBackupChildren, {
    name: "list-children",
    description: "List the external-service child backups that belong to a parent backup",
    path: { id: 'int' },
  })
}
