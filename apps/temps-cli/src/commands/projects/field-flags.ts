// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Typed flags for the project request fields the hand-written `projects`
 * commands have no dedicated option for (see `fieldFlags` in
 * `src/lib/endpoint.ts`). Each set is added to one command and read at the
 * start of its action; a flag left out leaves the setting unchanged.
 */

import type {
  CreateProjectRequest,
  ShieldMode,
  UpdateGitSettingsData,
  UpdateProjectData,
  UpdateProjectDeploymentConfigData,
  UpdateProjectSettingsData,
} from '../../api/types.gen.js'
import { fieldFlags } from '../../lib/endpoint.js'

const SHIELD_MODES: ShieldMode[] = ['off', 'log', 'selective', 'attack']

/** `projects create`: switches the creation request takes beyond the wizard's questions. */
export const CREATE_PROJECT_EXTRA = fieldFlags<CreateProjectRequest>({
  cloudflare_enabled: { kind: 'bool', help: "Serve through Cloudflare (overrides the instance default)" },
  custom_domain: { kind: 'string', help: 'Custom domain to attach to the production environment' },
  expected_slug: { kind: 'string', help: 'Fail unless the project gets exactly this URL slug' },
  is_on_demand: { kind: 'bool', help: 'Start containers on the first request and stop them when idle' },
  is_public_repo: { kind: 'bool', help: 'The repository is public (cloned without a git connection)' },
  is_web_app: { kind: 'bool', help: 'The app serves HTTP (false for a worker with no port)' },
  performance_metrics_enabled: { kind: 'bool', help: 'Collect web performance metrics' },
  use_default_wildcard: { kind: 'bool', help: "Give environments a hostname under the instance's wildcard domain" },
})

/** `projects update`: the rest of the project record. */
export const UPDATE_PROJECT_EXTRA = fieldFlags<NonNullable<UpdateProjectData['body']>>({
  build_command: { kind: 'string', help: 'Build command (overrides the preset)' },
  install_command: { kind: 'string', help: 'Install command (overrides the preset)' },
  output_dir: { kind: 'string', help: 'Build output directory (static sites)' },
  cloudflare_enabled: { kind: 'bool', help: 'Serve through Cloudflare' },
  custom_domain: { kind: 'string', help: 'Custom domain of the production environment' },
  expected_slug: { kind: 'string', help: 'Fail unless the project keeps exactly this URL slug' },
  is_on_demand: { kind: 'bool', help: 'Start containers on the first request and stop them when idle' },
  is_public_repo: { kind: 'bool', help: 'The repository is public (cloned without a git connection)' },
  is_web_app: { kind: 'bool', help: 'The app serves HTTP (false for a worker with no port)' },
  performance_metrics_enabled: { kind: 'bool', help: 'Collect web performance metrics' },
  preset_config: { kind: 'json', help: 'Preset-specific build settings as JSON, e.g. {"buildCommand":"bun run build"}' },
  use_default_wildcard: { kind: 'bool', help: "Give environments a hostname under the instance's wildcard domain" },
})

/** `projects settings`: AI summaries, tracing, error source context, log budgets, previews and Shield. */
export const SETTINGS_EXTRA = fieldFlags<NonNullable<UpdateProjectSettingsData['body']>>({
  ai_alert_summaries_enabled: { kind: 'bool', help: 'Summarize metric alert notifications with AI' },
  ai_api_traffic_summary_enabled: { kind: 'bool', help: 'Summarize API traffic analytics with AI' },
  cross_project_trace_sharing: { kind: 'bool', help: "Show this project's spans in other projects' traces" },
  error_source_context_enabled: { kind: 'bool', help: 'Show source code in error stack traces (uploads source files)' },
  error_source_root: { kind: 'string', help: 'Source root for error source context, relative to the checkout' },
  log_ingest_lines_per_sec: { kind: 'int', help: "Log ingestion budget for the project's containers, lines per second (0 lifts it)" },
  log_ingest_kib_per_sec: { kind: 'int', help: 'Log ingestion budget in KiB per second (0 lifts it)' },
  preview_envs_on_demand: { kind: 'bool', help: 'New preview environments start on demand and stop when idle' },
  preview_envs_idle_timeout_seconds: { kind: 'int', help: 'Idle seconds (60-86400) before an on-demand preview stops' },
  preview_envs_wake_timeout_seconds: { kind: 'int', help: 'Seconds (5-120) to wait for an on-demand preview to wake' },
  serves_own_temps_api: { kind: 'bool', help: "Send /api/_temps* and /api/otel* on the project's domains to the app instead of Temps" },
  shield_mode: { kind: 'string', help: 'How Shield acts on its verdicts', values: SHIELD_MODES },
})

/** `projects git`: public repositories, preset settings and sparse checkout. */
export const GIT_EXTRA = fieldFlags<NonNullable<UpdateGitSettingsData['body']>>({
  is_public_repo: { kind: 'bool', help: 'The repository is public: clone it without a git connection' },
  git_url: { kind: 'string', help: 'Clone URL of a public repository' },
  preset_config: { kind: 'json', help: 'Preset-specific build settings as JSON, e.g. {"buildCommand":"bun run build"}' },
  pull_only_root_directory: { kind: 'bool', help: 'Clone only --directory instead of the whole repository' },
})

/** `projects config`: build placement, requests, health, timeouts, load balancing and telemetry. */
export const DEPLOYMENT_CONFIG_EXTRA = fieldFlags<NonNullable<UpdateProjectDeploymentConfigData['body']>>({
  buildLocation: { kind: 'string', help: "Where a source deployment's image is built", values: ['control_plane', 'node', 'ci'] },
  crossArchitectureBuilds: { kind: 'bool', help: 'Build one image per architecture the eligible nodes run' },
  cpuRequest: { kind: 'int', help: 'CPU reserved per container, in microcores' },
  memoryRequest: { kind: 'int', help: 'Memory reserved per container, in MB' },
  exposedPort: { kind: 'int', help: 'Port the container listens on' },
  health: { kind: 'json', help: 'Health check settings as JSON, e.g. {"path":"/healthz"}' },
  healthCheckTimeoutSeconds: { kind: 'int', help: "Seconds (30-3600) a new deployment's containers get to become ready" },
  stopTimeoutSeconds: { kind: 'int', help: "Seconds (1-3600) the previous deployment gets to finish in-flight requests" },
  loadBalancing: { kind: 'json', help: 'Load balancing settings as JSON' },
  maxConcurrentConnections: { kind: 'int', help: "Default cap on concurrent requests to an environment's upstream (0 = unlimited)" },
  performanceMetricsEnabled: { kind: 'bool', help: 'Collect web performance metrics' },
  sessionRecordingEnabled: { kind: 'bool', help: 'Record visitor sessions' },
})
