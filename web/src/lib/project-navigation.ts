// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const PROJECT_PRIMARY_ROUTES = [
  'project',
  'deployments',
  'environments',
  'logs',
  'errors',
  'traces',
  'analytics',
  'monitoring',
  'storage',
  'security',
  'settings',
] as const
export type ProjectSection = (typeof PROJECT_PRIMARY_ROUTES)[number]
export interface ProjectSectionLink {
  title: string
  url: string
  aliases?: string[]
}
export const PROJECT_SECTION_LINKS: Partial<
  Record<ProjectSection, ProjectSectionLink[]>
> = {
  project: [
    { title: 'Overview', url: 'project' },
    { title: 'Activity', url: 'observe' },
  ],
  logs: [
    { title: 'Application logs', url: 'runtime' },
    { title: 'Request logs', url: 'request-logs', aliases: ['logs'] },
    { title: 'Telemetry logs', url: 'telemetry-logs' },
  ],
  errors: [{ title: 'Issues', url: 'errors' }],
  traces: [
    { title: 'Traces', url: 'traces' },
    { title: 'AI traces', url: 'ai-gateway?tab=activity' },
  ],
  monitoring: [
    { title: 'Metrics', url: 'metrics', aliases: ['dashboards'] },
    { title: 'Uptime', url: 'monitors' },
    { title: 'Alert rules', url: 'errors/alert-rules' },
  ],
  analytics: [
    { title: 'Overview', url: 'analytics' },
    { title: 'Visitors', url: 'analytics/visitors' },
    { title: 'Pages', url: 'analytics/pages' },
    { title: 'Sessions', url: 'analytics/replays' },
    { title: 'Funnels', url: 'analytics/funnels' },
    { title: 'Web performance', url: 'speed' },
    { title: 'AI visitors', url: 'analytics/ai-agents' },
    { title: 'API traffic', url: 'analytics/api-traffic' },
    { title: 'AI crawlers', url: 'ai-crawlers' },
    { title: 'Revenue', url: 'revenue' },
  ],
  storage: [
    { title: 'Databases', url: 'storage', aliases: ['databases'] },
    { title: 'Key-value store', url: 'services/kv' },
    { title: 'Blob storage', url: 'services/blob' },
  ],
  security: [
    { title: 'Protection', url: 'settings/security' },
    { title: 'Access', url: 'settings/access' },
    { title: 'Scans', url: 'security' },
  ],
  settings: [
    {
      title: 'General',
      url: 'settings/general',
      aliases: ['settings', 'setup'],
    },
    {
      title: 'Build & deploy',
      url: 'settings/delivery',
      aliases: [
        'build',
        'settings/build',
        'git',
        'settings/git',
        'connect-repository',
        'flags',
      ],
    },
    { title: 'Domains', url: 'domains', aliases: ['settings/domains'] },
    {
      title: 'Variables',
      url: 'settings/variables',
      aliases: [
        'environment-variables',
        'settings/environment-variables',
        'settings/secrets',
        'settings/deployment-tokens',
      ],
    },
    {
      title: 'Automation',
      url: 'settings/automation',
      aliases: ['agents', 'autofixer', 'settings/cron-jobs'],
    },
    {
      title: 'Integrations',
      url: 'settings/integrations',
      aliases: ['settings/webhooks', 'settings/skills', 'settings/mcp-servers'],
    },
    { title: 'Telemetry', url: 'settings/telemetry' },
  ],
}
const matches = (route: string, prefix: string) =>
  route === prefix || route.startsWith(`${prefix}/`)
export function resolveProjectPrimaryRoute(
  activeRoute: string
): ProjectSection {
  const route = activeRoute.split('?')[0]
  if (!route || route === 'project') return 'project'
  if (matches(route, 'deployments') || route === 'drop') return 'deployments'
  if (matches(route, 'environments')) return 'environments'
  for (const section of [
    'security',
    'monitoring',
    'logs',
    'errors',
    'traces',
    'analytics',
    'project',
    'storage',
  ] as const) {
    if (
      PROJECT_SECTION_LINKS[section]?.some((link) =>
        [link.url.split('?')[0], ...(link.aliases ?? [])].some((prefix) =>
          matches(route, prefix)
        )
      )
    )
      return section
  }
  return 'settings'
}
export function resolveProjectSectionLink(
  section: ProjectSection,
  route: string,
  _search: string
): string | undefined {
  return PROJECT_SECTION_LINKS[section]
    ?.flatMap((link) =>
      [link.url.split('?')[0], ...(link.aliases ?? [])]
        .filter((prefix) => matches(route, prefix))
        .map((prefix) => ({ url: link.url, length: prefix.length }))
    )
    .sort((a, b) => b.length - a.length)[0]?.url
}
/** Legacy tools URLs now belong to the flat Settings section. */
export function isProjectToolsRoute(activeRoute: string): boolean {
  return activeRoute === 'tools'
}
