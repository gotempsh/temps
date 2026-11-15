// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Project settings live on a handful of combined pages, each a stack of
 * collapsed sections. This module is the one map from "a setting" to "the URL
 * that opens it": the `?section=` id every section answers to, and where each
 * of the older standalone routes now lives, so links, the command palette and
 * redirects cannot disagree about it.
 */

export type CombinedSettingsPage =
  | 'general'
  | 'delivery'
  | 'variables'
  | 'deployment-tokens'
  | 'automation'
  | 'integrations'

/** Query parameter that opens and scrolls to one section of a combined page. */
export const SETTINGS_SECTION_PARAM = 'section'

/** Stable section ids per page. Part of the URL contract — never rename one. */
export const SETTINGS_SECTION_IDS = {
  general: ['project'],
  delivery: [
    'source',
    'repository',
    'build',
    'deployment',
    'previews',
    'feature-flags',
    'host-access',
  ],
  // `environment-variables` has its own page now; the id stays so old
  // `?section=` links still parse. Deployment tokens have their own page.
  variables: ['environment-variables', 'secrets'],
  'deployment-tokens': ['deployment-tokens'],
  // `agents` is the old "Agents & runs" section; it now opens the automation
  // list, which holds the repository workflows.
  automation: ['automations', 'agents', 'templates', 'chats', 'crash-autofix'],
  integrations: ['webhooks', 'skills', 'mcp-servers', 'extensions'],
} as const satisfies Record<CombinedSettingsPage, readonly string[]>

export type SettingsSectionId<P extends CombinedSettingsPage> =
  (typeof SETTINGS_SECTION_IDS)[P][number]

/** Project-relative path to one section, e.g. `settings/automation?section=chats`. */
export function settingsSectionPath<P extends CombinedSettingsPage>(
  page: P,
  section: SettingsSectionId<P>
): string {
  return `settings/${page}?${SETTINGS_SECTION_PARAM}=${section}`
}

/** Absolute console path to one section of a project's settings. */
export function settingsSectionHref<P extends CombinedSettingsPage>(
  projectSlug: string,
  page: P,
  section: SettingsSectionId<P>
): string {
  return `/projects/${projectSlug}/${settingsSectionPath(page, section)}`
}

/** DOM id of a section's disclosure, namespaced so it cannot collide. */
export function settingsSectionDomId(section: string): string {
  return `settings-section-${section}`
}

/**
 * The section a `?section=` value asks for on `page`, or `undefined` when the
 * value is missing or names a section that page does not have.
 */
export function requestedSettingsSection(
  page: CombinedSettingsPage,
  search: string | URLSearchParams
): string | undefined {
  const params =
    typeof search === 'string' ? new URLSearchParams(search) : search
  const requested = params.get(SETTINGS_SECTION_PARAM)
  return requested &&
    (SETTINGS_SECTION_IDS[page] as readonly string[]).includes(requested)
    ? requested
    : undefined
}

/**
 * Standalone routes that predate the combined pages, keyed by their
 * project-relative path. Each now redirects to its canonical home. Detail
 * routes below them (a secret, a cron job, a webhook, an agent run) are still
 * real pages and are not listed here.
 */
export const LEGACY_PROJECT_ROUTES = {
  flags: settingsSectionPath('delivery', 'feature-flags'),
  git: settingsSectionPath('delivery', 'repository'),
  'settings/git': settingsSectionPath('delivery', 'repository'),
  build: settingsSectionPath('delivery', 'build'),
  'settings/build': settingsSectionPath('delivery', 'build'),
  // The production-readiness checklist is no longer part of settings.
  setup: 'project',
  agents: settingsSectionPath('automation', 'agents'),
  autofixer: settingsSectionPath('automation', 'crash-autofix'),
  // Cron jobs call your app over HTTP; they live under Monitoring.
  'settings/cron-jobs': 'cron-jobs',
  'settings/webhooks': settingsSectionPath('integrations', 'webhooks'),
  'settings/skills': settingsSectionPath('integrations', 'skills'),
  'settings/mcp-servers': settingsSectionPath('integrations', 'mcp-servers'),
  'settings/secrets': settingsSectionPath('variables', 'secrets'),
  'settings/domains': 'domains',
  'settings/environment-variables': 'environment-variables',
} as const

export type LegacyProjectRoute = keyof typeof LEGACY_PROJECT_ROUTES

/** The old build page picked its sub-page with `?tab=`; each is now a section. */
const BUILD_TAB_SECTIONS: Record<string, SettingsSectionId<'delivery'>> = {
  source: 'source',
  build: 'build',
  deploy: 'deployment',
  previews: 'previews',
}

/**
 * Project-relative redirect target for a legacy route. Query parameters the
 * old page understood are carried over (a filter, a tab inside the page), so
 * a bookmarked link keeps its meaning; the old build page's `?tab=` becomes
 * the matching section.
 */
export function legacyProjectRouteTarget(
  route: LegacyProjectRoute,
  search: string
): string {
  const [path, query = ''] = LEGACY_PROJECT_ROUTES[route].split('?')
  const target = new URLSearchParams(query)
  const carried = new URLSearchParams(search)
  if (route === 'build' || route === 'settings/build') {
    const section = BUILD_TAB_SECTIONS[carried.get('tab') ?? '']
    if (section) target.set(SETTINGS_SECTION_PARAM, section)
    carried.delete('tab')
  }
  carried.forEach((value, key) => {
    if (!target.has(key)) target.append(key, value)
  })
  const qs = target.toString()
  return qs ? `${path}?${qs}` : path
}

/**
 * Rewrite a project URL whose first segment was renamed, keeping everything
 * after it: `/projects/app/logs/42?ts=1` → `/projects/app/request-logs/42?ts=1`.
 * Works on the raw (still percent-encoded) pathname so an encoded id survives.
 * Returns `undefined` when `pathname` is not under `from`.
 */
export function renameProjectRoutePrefix(
  pathname: string,
  from: string,
  to: string
): string | undefined {
  const match = /^(\/projects\/[^/]+)\/([^/]+)(\/.*)?$/.exec(pathname)
  if (!match || match[2] !== from) return undefined
  return `${match[1]}/${to}${match[3] ?? ''}`
}
