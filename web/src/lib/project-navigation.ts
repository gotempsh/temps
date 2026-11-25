// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Navigation model for a project (`/projects/:slug/*`): one menu grouped by
 * job (Ship, Observe, Configure), each section's pages shown as underline
 * tabs above the page. A tab's URL is the page's sub-path below the project
 * (`deployments`, `errors/feedback`, …); `aliases` are older or nested paths
 * that belong to the same tab.
 */

/** How the shared environment bar applies to a section's pages. */
export type EnvironmentScope =
  /** The page shows one environment; the bar picks which. */
  | 'single'
  /** The page lists records; the bar can also select "All environments". */
  | 'all'
  /** The page is project-wide; the bar is informational. */
  | 'project'

import type { PageWidth } from '@temps-sdk/ds'
import {
  Activity,
  AlertOctagon,
  HardDrive,
  ArrowLeftRight,
  BarChart3,
  BellRing,
  Bot,
  CalendarCheck,
  CalendarRange,
  CircleDollarSign,
  CirclePlay,
  Clock,
  Database,
  Earth,
  FileCode,
  FileText,
  Filter,
  MessageSquareText,
  Gauge,
  GitBranch,
  GitFork,
  Globe,
  Hammer,
  HeartPulse,
  History,
  Home,
  KeyRound,
  Layers,
  LayoutGrid,
  ListFilter,
  Lock,
  MessageSquare,
  Network,
  Paperclip,
  Plug,
  Puzzle,
  Radar,
  Radio,
  Route,
  ScrollText,
  Settings,
  ShieldCheck,
  SlidersHorizontal,
  Sparkles,
  Tag,
  Ticket,
  UserCheck,
  UserCog,
  Users,
  Variable,
  Workflow,
  type LucideIcon,
} from 'lucide-react'

/** How wide a page may grow; see `PageWidth` in the design system. */
export type { PageWidth }

/** A page behind a tab that holds several: a row of a second strip. */
export interface ProjectNavView {
  title: string
  /** Distinct within its tab; the first view shares the tab's icon. */
  icon: LucideIcon
  /** Overrides the tab's and the section's width for this page. */
  width?: PageWidth
  /** Sub-path below the project root (`/projects/:slug/<url>`). */
  url: string
  /** Other sub-path prefixes that belong to this page (detail pages, legacy). */
  aliases?: string[]
  /** Overrides the section's scope for this page. */
  environmentScope?: EnvironmentScope
}

export interface ProjectNavTab extends ProjectNavView {
  /**
   * The tab's pages, when it holds more than one. They show as a second
   * strip under the section's, so a section keeps a few tabs to look at and
   * its narrower pages stay one click further. The first is the tab's own
   * `url`.
   */
  views?: [ProjectNavView, ProjectNavView, ...ProjectNavView[]]
}

export type ProjectSectionId =
  | 'overview'
  | 'deployments'
  | 'logs'
  | 'errors'
  | 'variables'
  | 'traces'
  | 'metrics'
  | 'uptime'
  | 'analytics'
  | 'alerts'
  | 'domains'
  | 'databases'
  | 'access'
  | 'automation'
  | 'general'

export interface ProjectNavSection {
  id: ProjectSectionId
  title: string
  /** The section's icon in the project menu. */
  icon: LucideIcon
  environmentScope: EnvironmentScope
  width: PageWidth
  /** The first tab is the section's landing page. */
  tabs: [ProjectNavTab, ...ProjectNavTab[]]
}

/** Sub-path of the Build & deploy page, which embeds the delivery settings. */
export const BUILD_AND_DEPLOY_PATH = 'build-deploy'

/** A tab that holds `views`; it opens on the first. */
function tabOf(
  title: string,
  views: [ProjectNavView, ProjectNavView, ...ProjectNavView[]],
  icon: LucideIcon = views[0].icon
): ProjectNavTab {
  return { title, icon, url: views[0].url, views }
}

/**
 * A service's views, in the order of the tabs of its page: what it is, the
 * data it has, what it owns, then how it is configured. One shape with a
 * project's, so a view is in the same place at both levels.
 */
export const PROJECT_NAV_SECTIONS: ProjectNavSection[] = [
  {
    id: 'overview',
    title: 'Overview',
    icon: Home,
    environmentScope: 'single',
    width: 'full',
    tabs: [
      {
        title: 'Overview',
        icon: Home,
        url: 'project',
        // A monitoring-only project's overview also answers `integrations`.
        aliases: ['integrations'],
      },
      { title: 'Activity', icon: History, url: 'activity' },
    ],
  },
  {
    id: 'deployments',
    title: 'Deployments',
    icon: GitBranch,
    // Its list filters by the bar, "All environments" included.
    environmentScope: 'all',
    width: 'full',
    tabs: [
      {
        title: 'Deployments',
        icon: GitBranch,
        url: 'deployments',
        aliases: ['drop'],
      },
      // Where a deployment runs, and how it is built: what is set up
      // around deploying, beside what was deployed.
      {
        title: 'Environments',
        icon: Layers,
        url: 'environments',
        environmentScope: 'single',
      },
      {
        title: 'Build',
        icon: Hammer,
        url: BUILD_AND_DEPLOY_PATH,
        width: 'form',
        environmentScope: 'single',
        aliases: [
          'settings/delivery',
          'settings/build',
          'settings/git',
          'build',
          'git',
          'connect-repository',
          'flags',
        ],
      },
    ],
  },
  {
    id: 'logs',
    title: 'Logs',
    icon: ScrollText,
    environmentScope: 'single',
    width: 'full',
    tabs: [
      { title: 'Application', icon: ScrollText, url: 'runtime' },
      {
        title: 'Requests',
        icon: ArrowLeftRight,
        url: 'request-logs',
        aliases: ['logs'],
      },
      // Logs received over OTLP. Named for the protocol: "Telemetry"
      // also named the ingest settings.
      { title: 'OpenTelemetry', icon: Radio, url: 'telemetry-logs' },
    ],
  },
  {
    id: 'errors',
    title: 'Errors',
    icon: AlertOctagon,
    // Its pages filter by the bar, "All environments" included.
    environmentScope: 'all',
    width: 'full',
    tabs: [
      { title: 'Issues', icon: AlertOctagon, url: 'errors' },
      { title: 'Feedback', icon: MessageSquare, url: 'errors/feedback' },
      { title: 'Releases', icon: Tag, url: 'releases' },
      // Set once, then left alone: behind one tab, not six.
      tabOf(
        'Settings',
        [
          {
            title: 'Source maps',
            icon: FileCode,
            url: 'errors/source-maps',
          },
          {
            title: 'Inbound filters',
            icon: ListFilter,
            url: 'errors/inbound-filters',
          },
          {
            title: 'Attachments',
            icon: Paperclip,
            url: 'errors/attachments',
          },
          { title: 'Ownership', icon: UserCheck, url: 'errors/ownership' },
          { title: 'SDK health', icon: HeartPulse, url: 'client-outcomes' },
          { title: 'Setup', icon: Plug, url: 'errors/setup' },
        ],
        SlidersHorizontal
      ),
    ],
  },
  {
    id: 'variables',
    title: 'Variables',
    icon: Variable,
    environmentScope: 'project',
    // Tables of keys and values, not forms.
    width: 'full',
    tabs: [
      {
        title: 'Variables',
        icon: Variable,
        url: 'environment-variables',
        aliases: ['settings/environment-variables'],
      },
      {
        title: 'Secrets',
        icon: KeyRound,
        url: 'settings/variables',
        aliases: ['settings/secrets'],
      },
      {
        title: 'Deploy tokens',
        icon: Ticket,
        url: 'settings/deployment-tokens',
      },
    ],
  },
  {
    id: 'traces',
    title: 'Traces',
    icon: GitFork,
    // Its pages filter by the bar, "All environments" included.
    environmentScope: 'all',
    width: 'full',
    tabs: [
      { title: 'Traces', icon: GitFork, url: 'traces' },
      { title: 'AI traces', icon: Sparkles, url: 'ai-gateway' },
    ],
  },
  {
    id: 'metrics',
    title: 'Metrics',
    icon: Activity,
    environmentScope: 'single',
    width: 'full',
    tabs: [
      {
        title: 'Metrics',
        icon: Activity,
        url: 'metrics/explore',
        aliases: ['metrics'],
      },
      {
        title: 'Dashboards',
        icon: LayoutGrid,
        url: 'metrics/dashboards',
        aliases: ['dashboards'],
      },
    ],
  },
  {
    id: 'uptime',
    title: 'Uptime',
    icon: HeartPulse,
    environmentScope: 'single',
    width: 'full',
    tabs: [
      { title: 'Monitors', icon: HeartPulse, url: 'monitors' },
      {
        title: 'Cron jobs',
        icon: Clock,
        url: 'cron-jobs',
        aliases: ['settings/cron-jobs'],
      },
      // Jobs scheduled elsewhere, reporting through the Sentry SDK: a
      // different feature from the jobs Temps runs, so its own tab.
      { title: 'Check-ins', icon: CalendarCheck, url: 'check-ins' },
      { title: 'Workflows', icon: Workflow, url: 'durable-workflows' },
    ],
  },
  {
    id: 'analytics',
    title: 'Analytics',
    icon: BarChart3,
    // Its pages filter by the bar, "All environments" included.
    environmentScope: 'all',
    width: 'full',
    tabs: [
      { title: 'Overview', icon: BarChart3, url: 'analytics' },
      tabOf('Visitors', [
        { title: 'Visitors', icon: Users, url: 'analytics/visitors' },
        { title: 'Live', icon: Radio, url: 'analytics/live-visitors' },
        { title: 'Globe', icon: Earth, url: 'analytics/live' },
        { title: 'Journey', icon: Route, url: 'analytics/journey' },
        {
          title: 'Sessions',
          icon: CirclePlay,
          url: 'analytics/replays',
          width: 'full',
        },
        { title: 'AI visitors', icon: Bot, url: 'analytics/ai-agents' },
        { title: 'AI crawlers', icon: Radar, url: 'ai-crawlers' },
      ]),
      tabOf('Pages', [
        { title: 'Pages', icon: FileText, url: 'analytics/pages' },
        { title: 'Web performance', icon: Gauge, url: 'speed' },
        {
          title: 'API traffic',
          icon: Network,
          url: 'analytics/api-traffic',
        },
      ]),
      tabOf('Funnels', [
        { title: 'Funnels', icon: Filter, url: 'analytics/funnels' },
        {
          title: 'Surveys',
          icon: MessageSquareText,
          url: 'analytics/surveys',
        },
      ]),
      { title: 'Revenue', icon: CircleDollarSign, url: 'revenue' },
      tabOf(
        'Settings',
        [
          {
            title: 'Traffic filters',
            icon: ListFilter,
            url: 'analytics/settings',
          },
          {
            title: 'Activity report',
            icon: CalendarRange,
            url: 'analytics/activity',
          },
        ],
        SlidersHorizontal
      ),
    ],
  },
  {
    // The rules of both kinds in one place. They are two lists with two
    // forms, so two tabs, not one merged list.
    id: 'alerts',
    title: 'Alerts',
    icon: BellRing,
    environmentScope: 'all',
    width: 'full',
    tabs: [
      { title: 'Error alerts', icon: BellRing, url: 'errors/alert-rules' },
      {
        title: 'Metric alerts',
        icon: Gauge,
        url: 'metrics/alerts',
        environmentScope: 'single',
      },
    ],
  },
  {
    id: 'domains',
    title: 'Domains',
    icon: Globe,
    environmentScope: 'project',
    width: 'full',
    tabs: [
      {
        title: 'Domains',
        icon: Globe,
        url: 'domains',
        aliases: ['settings/domains'],
      },
    ],
  },
  {
    id: 'databases',
    title: 'Databases',
    icon: Database,
    environmentScope: 'project',
    width: 'full',
    tabs: [
      {
        title: 'Databases',
        icon: Database,
        url: 'storage',
        aliases: ['databases'],
      },
      { title: 'Key-value', icon: KeyRound, url: 'services/kv' },
      { title: 'Blob', icon: HardDrive, url: 'services/blob' },
    ],
  },
  {
    id: 'access',
    title: 'Security',
    icon: Lock,
    environmentScope: 'project',
    width: 'form',
    tabs: [
      { title: 'Protection', icon: Lock, url: 'settings/security' },
      {
        title: 'Edge rules',
        icon: ListFilter,
        url: 'settings/edge-rules',
        width: 'full',
      },
      { title: 'Access', icon: UserCog, url: 'settings/access' },
      { title: 'Scans', icon: ShieldCheck, url: 'security', width: 'full' },
    ],
  },
  {
    id: 'automation',
    title: 'Automation',
    icon: Workflow,
    environmentScope: 'project',
    width: 'form',
    tabs: [
      {
        title: 'Automation',
        icon: Workflow,
        url: 'settings/automation',
        aliases: ['agents', 'autofixer'],
      },
      {
        title: 'Integrations',
        icon: Puzzle,
        url: 'settings/integrations',
        aliases: [
          'settings/webhooks',
          'settings/skills',
          'settings/mcp-servers',
        ],
      },
    ],
  },
  {
    id: 'general',
    title: 'Settings',
    icon: Settings,
    environmentScope: 'project',
    width: 'form',
    tabs: [
      {
        title: 'General',
        icon: Settings,
        url: 'settings/general',
        aliases: ['settings', 'tools'],
      },
      // How this service's SDKs send data in. Was "Telemetry", a name
      // the OTLP logs tab also had.
      { title: 'Data ingest', icon: Radio, url: 'settings/telemetry' },
    ],
  },
]

function matchesPrefix(route: string, prefix: string): boolean {
  return route === prefix || route.startsWith(`${prefix}/`)
}

export interface ProjectNavLocation {
  section: ProjectNavSection
  tab: ProjectNavTab
  /** The tab's page shown, when the tab holds several. */
  view?: ProjectNavView
}

/** The pages behind a tab: its views, or the tab itself. */
function pagesOf(tab: ProjectNavTab): readonly ProjectNavView[] {
  return tab.views ?? [tab]
}

/**
 * The section and tab a sub-path belongs to. The longest matching prefix
 * wins, so `errors/alert-rules` resolves to Alerts rather than Errors and
 * `metrics/dashboards/3` to Dashboards rather than Metrics. An unknown
 * sub-path falls back to the Overview so the menu always highlights something.
 */
export function resolveProjectLocation(route: string): ProjectNavLocation {
  const normalized = route.replace(/^\/+|\/+$/g, '')
  const overview = PROJECT_NAV_SECTIONS[0]
  let best: (ProjectNavLocation & { length: number }) | undefined
  for (const section of PROJECT_NAV_SECTIONS) {
    for (const tab of section.tabs) {
      for (const page of pagesOf(tab)) {
        for (const prefix of [page.url, ...(page.aliases ?? [])]) {
          if (
            matchesPrefix(normalized, prefix) &&
            (!best || prefix.length > best.length)
          ) {
            best = {
              section,
              tab,
              view: tab.views ? page : undefined,
              length: prefix.length,
            }
          }
        }
      }
    }
  }
  if (best) return { section: best.section, tab: best.tab, view: best.view }
  return { section: overview, tab: overview.tabs[0] }
}

/**
 * Whether the tabs above already name the page at `route`: it is a view's,
 * a tab's or a sub-page's own page, not a record below it (`errors`, not
 * `errors/42`). The page then does not print its title a second time. The
 * Overview is the exception: its tab says "Overview", its page names the
 * service.
 */
export function projectTabNamesPage(
  route: string,
  location: ProjectNavLocation
): boolean {
  if (location.section.id === 'overview') return false
  const normalized = route.replace(/^\/+|\/+$/g, '')
  const page = location.view ?? location.tab
  return [page.url, ...(page.aliases ?? [])].includes(normalized)
}

/** How the environment bar applies to the page at `location`. */
export function projectEnvironmentScope(
  location: ProjectNavLocation
): EnvironmentScope {
  return (
    location.view?.environmentScope ??
    location.tab.environmentScope ??
    location.section.environmentScope
  )
}

/** Root URL of a project's pages. */
export function projectRoot(slug: string): string {
  return `/projects/${slug}`
}

/** Query value of the environments page's settings view. */
const ENVIRONMENT_SETTINGS_VIEW = 'settings'

/**
 * The width a project page gets. The environment settings view is a form,
 * though the rest of the Environments tab is a dashboard.
 */
export function projectPageWidth(
  location: ProjectNavLocation,
  search: string
): PageWidth {
  const width = location.view?.width ?? location.tab.width
  if (width) return width
  if (
    location.tab.url === 'environments' &&
    new URLSearchParams(search).get('view') === ENVIRONMENT_SETTINGS_VIEW
  )
    return 'form'
  return location.section.width
}

/**
 * Project-relative pages a monitoring-only (`source_type: 'external'`)
 * project registers — see the external route table in `ProjectDetail`.
 * Everything else is a hosting page (deployments, variables, delivery
 * settings, ...) that such a project answers with "Add hosting", so the
 * command palette does not offer it as a destination.
 */
const EXTERNAL_PROJECT_PAGES = [
  'project',
  'hosting',
  'analytics',
  'speed',
  'revenue',
  'errors',
  'traces',
  'ai-gateway',
  'telemetry-logs',
  'releases',
  'client-outcomes',
  'metrics',
  'monitors',
  'check-ins',
  'status-pages',
  'settings/general',
  'settings/telemetry',
] as const

/** Whether a project-relative URL is a page an external project has. */
export function isExternalProjectPage(url: string): boolean {
  const route = url.split(/[?#]/)[0]
  return EXTERNAL_PROJECT_PAGES.some((page) => matchesPrefix(route, page))
}

/** Whether `url` is a page `project` has, given its kind. */
export function projectHasPage(
  project: { source_type?: string | null },
  url: string
): boolean {
  return project.source_type !== 'external' || isExternalProjectPage(url)
}

/** `tab` with only the pages `project` has, or nothing when it has none. */
function tabFor(
  project: { source_type?: string | null },
  tab: ProjectNavTab
): ProjectNavTab[] {
  if (!tab.views) return projectHasPage(project, tab.url) ? [tab] : []
  const views = tab.views.filter((view) => projectHasPage(project, view.url))
  const [first, second, ...rest] = views
  if (!first) return []
  // One page left needs no second strip: the tab is that page.
  if (!second) return [{ ...first, title: tab.title, icon: tab.icon }]
  return [{ ...tab, url: first.url, views: [first, second, ...rest] }]
}

/** `section` with only the tabs `project` has, or nothing when it has none. */
function sectionFor(
  project: { source_type?: string | null },
  section: ProjectNavSection
): ProjectNavSection[] {
  const [first, ...rest] = section.tabs.flatMap((tab) => tabFor(project, tab))
  return first ? [{ ...section, tabs: [first, ...rest] }] : []
}

/**
 * The views `project` actually has: a monitoring-only project keeps only
 * the pages it registers, and tabs and views left without one disappear.
 */
export function projectNavSectionsFor(project: {
  source_type?: string | null
}): ProjectNavSection[] {
  if (project.source_type !== 'external') return PROJECT_NAV_SECTIONS
  return PROJECT_NAV_SECTIONS.flatMap((section) => sectionFor(project, section))
}

/** The sections that are one view, Settings: its own page first. */
const SETTINGS_SECTIONS: readonly ProjectSectionId[] = [
  'general',
  'access',
  'automation',
]

/** A tab of a service's page: one section, or the settings ones together. */
export interface ProjectEntityView {
  /** The section's id; Settings goes by `general`. */
  id: ProjectSectionId
  title: string
  icon: LucideIcon
  /** The sections whose pages it holds: one, except for Settings. */
  sections: ProjectNavSection[]
  /** Its pages, as the row under the tabs. The first is where it opens. */
  tabs: ProjectNavTab[]
}

/**
 * The tabs of `project`'s page: a view per section, in order, with what is
 * configured once (security, automation, the service's own settings)
 * together under one Settings tab at the end, so the strip stays a list of
 * what is looked at every day.
 */
export function projectEntityViewsFor(project: {
  source_type?: string | null
}): ProjectEntityView[] {
  const sections = projectNavSectionsFor(project)
  const views = sections
    .filter((section) => !SETTINGS_SECTIONS.includes(section.id))
    .map((section): ProjectEntityView => ({
      id: section.id,
      title: section.title,
      icon: section.icon,
      sections: [section],
      tabs: section.tabs,
    }))
  const settings = SETTINGS_SECTIONS.flatMap((id) =>
    sections.filter((section) => section.id === id)
  )
  const [first] = settings
  if (first)
    views.push({
      id: 'general',
      title: first.title,
      icon: first.icon,
      sections: settings,
      tabs: settings.flatMap((section) => section.tabs),
    })
  return views
}

/**
 * Where `route` sits in `project`'s own menu: like `resolveProjectLocation`,
 * with the section's tabs narrowed to the pages the project has.
 */
export function projectLocationFor(
  project: { source_type?: string | null },
  route: string
): ProjectNavLocation {
  const location = resolveProjectLocation(route)
  if (project.source_type !== 'external') return location
  const [section] = sectionFor(project, location.section)
  if (!section) return location
  const tab =
    section.tabs.find((candidate) =>
      pagesOf(candidate).some(
        (page) => page.url === (location.view ?? location.tab).url
      )
    ) ?? location.tab
  return {
    section,
    tab,
    view: tab.views?.find((view) => view.url === location.view?.url),
  }
}
