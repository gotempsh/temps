// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { AppSettingsWritable } from '../../api/types.gen.js'

/**
 * Every top-level instance setting, with what it controls. Keyed by every
 * field of `AppSettingsWritable` (the PUT body), so a setting added to the API fails the typecheck
 * until it is described here, and `settings set` can refuse a misspelt one.
 */
export const SETTING_FIELDS = {
  agent_sandbox: 'Agent sandbox image and resources',
  ai_chat_limits: 'Limits on a single AI chat turn',
  ai_config: 'Global AI configuration',
  ai_workspace_file_limits: 'Transfer and preview limits for files in AI workspaces',
  analytics_retention: 'Retention of product analytics data and personal identifiers',
  anonymous_telemetry_enabled: 'Anonymous product telemetry preference (also: settings telemetry)',
  backup_executor: 'Backup executor concurrency',
  blob: 'Managed Blob service settings',
  build_limits: 'CPU and memory limits for builds on the control plane',
  bunny_new_projects: 'Enable Bunny delivery by default for new projects',
  clickhouse: 'ClickHouse wire compression',
  cloud: 'Managed control-plane connection',
  cloudflare_new_projects: 'Enable Cloudflare delivery by default for new projects',
  cluster_dns: 'Cluster DNS resolver (beta)',
  connection_limits: 'Per-upstream concurrent-connection cap in the proxy',
  console_force_https: 'Redirect the console host to HTTPS (null: once a certificate exists)',
  console_version: 'Console binary version tag',
  container_logs: 'Docker log rotation for containers (max size and files)',
  control_plane_resources: "The control plane's own scheduling settings",
  disk_space_alert: 'Disk usage alerting',
  dns_provider: 'DNS provider used for certificates and records',
  docker_registry: 'Docker registry deployments push to',
  edge_target: 'Edge target generated DNS records point at',
  environment_hostname_template: 'Hostname label of a new environment, from {project} and {env}',
  expose_platform_identity: 'Identify the platform in proxy responses',
  external_url: 'Public URL of this Temps console',
  geo: 'Geolocation database refresh policy and MaxMind credential',
  image_retention: 'Retention of locally built deployment images',
  insecure_tls: 'Skip TLS verification on outbound clients (unsafe)',
  internal_url: 'URL containers use to reach the Temps API inside Docker',
  kv: 'Managed KV service settings',
  letsencrypt: "Let's Encrypt account email and environment",
  loopback_ja4_header: "Header a trusted loopback proxy puts the visitor's JA4 fingerprint in",
  managed_service_healthchecks: 'Default Docker healthcheck of managed service containers',
  mcp_server: 'MCP server settings',
  monitoring: 'Metrics observability settings',
  multi_node: 'Multi-node cluster settings',
  observability_compression: 'Compression delays for observability data',
  observability_retention: 'Retention of raw proxy and OpenTelemetry data',
  on_demand_tls: 'On-demand HTTP-01 certificate issuance',
  otel_ingest: 'OTLP ingest limits, quota, analysis jobs and S3 log archival',
  plugin_installation_reporting_enabled: 'Share verified plugin installation counts with the registry',
  preview_domain: 'Domain preview hostnames are generated under',
  preview_gateway: 'Workspace preview gateway (also: temps preview-gateway)',
  production_hostname_template: 'Hostname label of an environment named production',
  public_ingress_url: 'Base URL third parties call back into (tracking, webhooks)',
  rate_limiting: 'Proxy rate limiting',
  registry_mirror_prefix: 'Mirror prefix for Docker Hub base images in generated builds',
  request_timeouts: 'Proxy request, SSE and WebSocket timeouts',
  require_mfa_for_admins: 'Require MFA for users with the Admin role',
  screenshots: 'Deployment screenshots',
  security_headers: 'Security headers added by the proxy',
  self_update: 'The console one-click update action',
  setup_complete: 'Whether initial setup has been applied',
  shield: 'Platform-wide Shield (bot and attack protection) settings',
  tenant_resource_ceilings: 'Ceilings on what a project may configure for itself',
  trust_loopback_forwarded_ip: 'Trust forwarded client-IP headers from a loopback peer',
  trusted_private_networks: 'Private networks user-configured outbound requests may reach',
  trusted_proxy_cidrs: 'Reverse proxies trusted to set forwarded client-IP headers',
} as const satisfies Record<keyof AppSettingsWritable, string>

/**
 * Settings that, once saved, can lock the caller out of the console or
 * silently weaken TLS; `settings set` asks before changing them.
 */
export const SENSITIVE_SETTINGS: ReadonlySet<string> = new Set([
  'external_url',
  'console_force_https',
  'require_mfa_for_admins',
  'insecure_tls',
  'trusted_proxy_cidrs',
  'trust_loopback_forwarded_ip',
])
