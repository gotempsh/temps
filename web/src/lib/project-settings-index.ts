// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  SETTINGS_SECTION_PARAM,
  settingsSectionDomId,
} from '@/lib/project-settings-sections'

/**
 * Every project setting someone might search for, and the exact place it
 * lives. The command palette indexes these, so typing "compose" or "health
 * check" lands on that setting, scrolled into view and highlighted, instead
 * of on the top of a long page.
 *
 * `anchor` is the DOM id of the group or row that holds the setting.
 * `section` names the always-present group around it, which the page scrolls
 * to first and keeps as the destination while the setting is still loading.
 * Settings that only exist for one build preset (the Compose file, the
 * Dockerfile path) name it in `preset`, and services built another way are
 * not offered them at all. `project-settings-index.test.ts` checks every anchor is still
 * rendered somewhere, so renaming one breaks the build rather than the link.
 *
 * Whole pages and the groups that already had palette entries (Shield, rate
 * limiting, webhooks, crash autofix…) stay in the palette's own list; this
 * holds the settings inside them.
 */
export interface ProjectSettingEntry {
  /** DOM id of the element to scroll to and highlight. */
  anchor: string
  title: string
  /** Project-relative page path, without a query. */
  page: string
  /** `?section=` id of the group around it, used as the fallback target. */
  section?: string
  /** Extra query parameters the page needs (e.g. `view=settings`). */
  query?: Record<string, string>
  /** Words people use for it, beyond the title. */
  keywords: string[]
  /** The only build preset whose services have this setting. */
  preset?: SettingPreset
}

/** A build preset some settings are specific to. */
export type SettingPreset = 'dockerfile' | 'docker-compose'

/**
 * The preset-specific settings a project's `preset` value has, if any. The
 * API spells Compose both `docker-compose` and `dockercompose`.
 */
export function settingPresetOf(
  preset: string | null | undefined
): SettingPreset | undefined {
  if (preset === 'dockerfile') return 'dockerfile'
  if (preset === 'docker-compose' || preset === 'dockercompose')
    return 'docker-compose'
  return undefined
}

/**
 * Whether `project` has the setting: always, unless the setting belongs to a
 * build preset the project does not use.
 */
export function projectHasSetting(
  project: { preset?: string | null },
  setting: { preset?: SettingPreset }
): boolean {
  return !setting.preset || setting.preset === settingPresetOf(project.preset)
}

/** DOM id prefix for a single indexed setting (a row or a card). */
export function settingAnchor(id: string): string {
  return `setting-${id}`
}

const GENERAL = 'settings/general'
const DELIVERY = 'settings/delivery'
const PROTECTION = 'settings/security'
const TELEMETRY = 'settings/telemetry'
const ENVIRONMENT = 'environments'
const ENVIRONMENT_SETTINGS = { view: 'settings' }

export const PROJECT_SETTINGS_INDEX: readonly ProjectSettingEntry[] = [
  // General
  {
    anchor: settingAnchor('identity'),
    title: 'Service name and slug',
    page: GENERAL,
    keywords: ['rename', 'name', 'slug', 'identity', 'url'],
  },
  {
    anchor: settingAnchor('cross-project-trace-sharing'),
    title: 'Cross-service trace sharing',
    page: GENERAL,
    keywords: ['traces', 'distributed tracing', 'cross project', 'telemetry'],
  },
  {
    anchor: settingAnchor('error-source-context'),
    title: 'Source code in stack traces',
    page: GENERAL,
    keywords: ['source context', 'stack trace', 'errors', 'source code'],
  },
  {
    anchor: settingAnchor('serves-own-temps-api'),
    title: 'App serves its own Temps API',
    page: GENERAL,
    keywords: ['temps api', 'proxy', 'api'],
  },
  {
    anchor: settingAnchor('danger-zone'),
    title: 'Delete service',
    page: GENERAL,
    keywords: ['delete', 'remove', 'danger zone', 'destroy'],
  },

  // Build & deploy
  {
    anchor: settingAnchor('where-it-runs'),
    title: 'Where it runs',
    page: DELIVERY,
    section: 'source',
    keywords: ['hosting', 'external', 'runs outside temps', 'self managed'],
  },
  {
    anchor: settingAnchor('deployment-source'),
    title: 'Deployment source',
    page: DELIVERY,
    section: 'source',
    keywords: ['docker image', 'registry', 'image', 'prebuilt', 'source'],
  },
  {
    anchor: settingAnchor('production-branch'),
    title: 'Production branch',
    page: DELIVERY,
    section: 'repository',
    keywords: ['branch', 'main', 'push to deploy', 'git branch'],
  },
  {
    anchor: settingAnchor('framework'),
    title: 'Framework',
    page: DELIVERY,
    section: 'build',
    keywords: [
      'preset',
      'nextjs',
      'vite',
      'dockerfile',
      'nixpacks',
      'buildpack',
      'language',
    ],
  },
  {
    anchor: settingAnchor('root-directory'),
    title: 'Root directory',
    page: DELIVERY,
    section: 'build',
    keywords: ['directory', 'monorepo', 'subfolder', 'path', 'folder'],
  },
  {
    anchor: settingAnchor('dockerfile-path'),
    title: 'Dockerfile path',
    page: DELIVERY,
    section: 'build',
    keywords: ['dockerfile', 'docker'],
    preset: 'dockerfile',
  },
  {
    anchor: settingAnchor('build-context'),
    title: 'Build context',
    page: DELIVERY,
    section: 'build',
    keywords: ['docker build', 'context', 'dockerfile'],
    preset: 'dockerfile',
  },
  {
    anchor: settingAnchor('compose-file'),
    title: 'Compose file',
    page: DELIVERY,
    section: 'build',
    keywords: ['docker-compose.yml', 'compose.yaml', 'docker compose'],
    preset: 'docker-compose',
  },
  {
    anchor: settingAnchor('docker-compose'),
    title: 'Docker Compose',
    page: DELIVERY,
    section: 'build',
    keywords: [
      'compose',
      'docker compose',
      'docker-compose',
      'services',
      'stack',
    ],
    preset: 'docker-compose',
  },
  {
    anchor: settingAnchor('compose-public-routes'),
    title: 'Compose public routes',
    page: DELIVERY,
    section: 'build',
    keywords: ['compose', 'ports', 'routes', 'expose', 'public port'],
    preset: 'docker-compose',
  },
  {
    anchor: settingAnchor('compose-services'),
    title: 'Compose services',
    page: DELIVERY,
    section: 'build',
    keywords: ['compose', 'exclude services', 'services', 'containers'],
    preset: 'docker-compose',
  },
  {
    anchor: settingAnchor('container-startup'),
    title: 'Container startup',
    page: DELIVERY,
    section: 'build',
    keywords: ['start command', 'entrypoint', 'cmd', 'command', 'startup'],
    preset: 'dockerfile',
  },
  {
    anchor: settingAnchor('resource-limits'),
    title: 'Default CPU and memory',
    page: DELIVERY,
    section: 'deployment',
    keywords: ['cpu', 'memory', 'ram', 'limits', 'requests', 'resources'],
  },
  {
    anchor: settingAnchor('replicas-and-port'),
    title: 'Default replicas and port',
    page: DELIVERY,
    section: 'deployment',
    keywords: ['replicas', 'instances', 'port', 'expose', 'scale'],
  },
  {
    anchor: 'health-probe',
    title: 'Health checks',
    page: DELIVERY,
    section: 'deployment',
    keywords: [
      'health probe',
      'health path',
      'healthcheck',
      'startup timeout',
      'liveness',
      'readiness',
    ],
  },
  {
    anchor: settingAnchor('build-location'),
    title: 'Build location',
    page: DELIVERY,
    section: 'deployment',
    keywords: ['build on', 'worker node', 'ci', 'control plane', 'builds'],
  },
  {
    anchor: settingAnchor('automatic-deployments'),
    title: 'Automatic deployments',
    page: DELIVERY,
    section: 'repository',
    keywords: ['auto deploy', 'deploy on push', 'automatic', 'push'],
  },
  {
    anchor: settingAnchor('port-overrides'),
    title: 'Environment port overrides',
    page: DELIVERY,
    section: 'deployment',
    keywords: ['port', 'override', 'environment port'],
  },
  {
    anchor: settingAnchor('image-retention'),
    title: 'Built image retention',
    page: DELIVERY,
    section: 'deployment',
    keywords: ['images', 'cleanup', 'disk', 'retention', 'prune'],
  },
  {
    anchor: settingAnchor('log-ingestion'),
    title: 'Log ingestion budget',
    page: DELIVERY,
    section: 'deployment',
    keywords: ['logs', 'budget', 'ingestion', 'log volume'],
  },
  {
    anchor: settingsSectionDomId('previews'),
    title: 'Preview environments',
    page: DELIVERY,
    section: 'previews',
    keywords: [
      'preview',
      'pull request',
      'branch deploys',
      'idle timeout',
      'wake',
      'scale to zero',
    ],
  },

  // Protection
  {
    anchor: 'shield-entries',
    title: 'Shield entries',
    page: PROTECTION,
    keywords: ['allowlist', 'blocklist', 'shield', 'block'],
  },
  {
    anchor: 'ai-assistance',
    title: 'AI alert summaries',
    page: PROTECTION,
    keywords: ['ai', 'summaries', 'alerts'],
  },

  // Telemetry
  {
    anchor: settingAnchor('span-storage'),
    title: 'Where spans are stored',
    page: TELEMETRY,
    keywords: ['spans', 'traces', 'cloud', 'local', 'storage'],
  },
  {
    anchor: settingAnchor('span-fidelity'),
    title: 'Span fidelity',
    page: TELEMETRY,
    keywords: ['spans', 'metadata', 'cloud', 'privacy'],
  },

  // Environment settings (the environment chosen in the bar)
  {
    anchor: settingAnchor('environment-git'),
    title: 'Environment branch and auto-deploy',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['branch', 'auto deploy', 'environment', 'git'],
  },
  {
    anchor: settingAnchor('environment-resources'),
    title: 'Environment CPU and memory',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['cpu', 'memory', 'ram', 'resources', 'limits', 'environment'],
  },
  {
    anchor: settingAnchor('environment-scaling'),
    title: 'Replicas (scaling)',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['replicas', 'instances', 'scale', 'horizontal'],
  },
  {
    anchor: settingAnchor('environment-scale-to-zero'),
    title: 'Scale to zero (on demand)',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['on demand', 'sleep', 'idle', 'wake', 'scale to zero'],
  },
  {
    anchor: settingAnchor('environment-timeouts'),
    title: 'Request timeouts',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['timeout', 'request timeout', 'proxy timeout'],
  },
  {
    anchor: settingAnchor('environment-connection-limits'),
    title: 'Connection limits',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['connections', 'concurrency', 'limits'],
  },
  {
    anchor: settingAnchor('environment-builds'),
    title: 'Environment builds',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['build', 'build location', 'environment'],
  },
  {
    anchor: settingAnchor('environment-node-scheduling'),
    title: 'Node scheduling',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['nodes', 'placement', 'anti affinity', 'pin', 'workers'],
  },
  {
    anchor: settingAnchor('environment-security'),
    title: 'Environment security and password',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: [
      'password protection',
      'basic auth',
      'shield',
      'security headers',
      'rate limit',
    ],
  },
  {
    anchor: settingAnchor('environment-subdomain'),
    title: 'Environment subdomain',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['subdomain', 'url', 'hostname'],
  },
  {
    anchor: settingAnchor('environment-volumes'),
    title: 'Volumes',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['volume', 'disk', 'persistent storage', 'mount'],
  },
  {
    anchor: settingAnchor('environment-access-control'),
    title: 'Environment access control',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['ingress', 'access control', 'private', 'sso'],
  },
  {
    anchor: settingAnchor('environment-asset-cache'),
    title: 'Asset cache',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['cache', 'purge', 'cdn', 'static assets'],
  },
  {
    anchor: settingAnchor('environment-danger-zone'),
    title: 'Delete environment',
    page: ENVIRONMENT,
    query: ENVIRONMENT_SETTINGS,
    keywords: ['delete environment', 'remove', 'danger zone'],
  },
]

/**
 * Project-relative URL of a setting: its page, the group to fall back to,
 * and the setting itself as the fragment, e.g.
 * `settings/delivery?section=build#setting-docker-compose`.
 */
export function projectSettingUrl(entry: ProjectSettingEntry): string {
  const params = new URLSearchParams(entry.query)
  if (entry.section) params.set(SETTINGS_SECTION_PARAM, entry.section)
  const query = params.toString()
  return `${entry.page}${query ? `?${query}` : ''}#${entry.anchor}`
}
