// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getProjectsInfiniteOptions,
  listGlobalMcpsOptions,
  listGlobalSkillsOptions,
  listProjectGroupsOptions,
  listServicesInfiniteOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { GROUP_PAGE_SIZE } from '@/components/project-groups/useProjectGroups'
import { ProjectAvatar } from '@/components/project/ProjectAvatar'
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
} from '@/components/ui/command'
import { usePluginsContext } from '@/contexts/PluginsContext'
import { useCanViewAuditLogs } from '@/hooks/useAuditAccess'
import { useFrecency } from '@/hooks/useFrecency'
import { normalizeFrecency } from '@/lib/frecency'
import { projectHasPage } from '@/lib/project-navigation'
import { settingsSectionPath } from '@/lib/project-settings-sections'
import {
  PROJECT_SETTINGS_INDEX,
  projectHasSetting,
  projectSettingUrl,
  type SettingPreset,
} from '@/lib/project-settings-index'
import {
  buildCommandSampleQueries,
  dedupeCommandDestinations,
  hoistResultFirst,
  resolveExplicitNamedProjectDestination,
  resolveExplicitProjectEnvironment,
  toCommandExtendedQuery,
  type CommandDestination,
} from '@/lib/command-navigation'
import {
  buildAccessibleNavigationMap,
  excludeNavigationUrls,
  filterRestrictedNavigationItems,
  isSettingsNavigationUrl,
  indexSettingsNavigationGroups,
  mergeNavigationItems,
  platformToolNavigationItems,
  settingsPageNavigationItems,
} from '@/lib/command-navigation-catalog'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { TERMS, groupHref } from '@/lib/terminology'
import { CLUSTER_DNS_URL, WORKER_NODES_URL } from '@/lib/worker-nodes'
import {
  mergeSettingsNavigationGroups,
  type SettingsNavigationIcon,
} from '@/components/settings/settings-navigation'
import { useConsoleExtensions } from '@temps-sdk/console-kit'
import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import Fuse from 'fuse.js'
import { NAV_FUSE_OPTIONS } from './nav-search'
import {
  Activity,
  BadgeCheck,
  BarChart3,
  Bell,
  BellPlus,
  Bot,
  Box,
  Boxes,
  Cloud,
  CreditCard,
  Database,
  DatabaseBackup,
  FileLock2,
  Flag,
  Folder,
  FolderPlus,
  Cpu,
  Gauge,
  GitBranch,
  Globe,
  HardDrive,
  History,
  Home,
  Key,
  KeyRound,
  Mail,
  MessageSquare,
  Monitor,
  Network,
  Puzzle,
  Rocket,
  ScrollText,
  Search,
  Server,
  Settings,
  Settings2,
  Shield,
  ShieldCheck,
  ShieldOff,
  Sparkles,
  SquareTerminal,
  SunMoon,
  Upload,
  Users,
  UsersRound,
  Wand2,
  Workflow,
  type LucideIcon,
} from 'lucide-react'
import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react'
import { useLocation, useNavigate } from 'react-router'

interface NavigationItem {
  title: string
  url: string
  icon: SettingsNavigationIcon
  keywords?: string[]
  /** Offered only for services built with this preset. */
  preset?: SettingPreset
}

export function CommandPaletteSuggestions({
  queries,
  onSelect,
}: {
  queries: string[]
  onSelect: (query: string) => void
}) {
  return (
    <CommandGroup heading="Suggested searches">
      {queries.map((query) => (
        <CommandItem
          key={query}
          value={`suggestion-${query}`}
          onSelect={() => onSelect(query)}
          className="gap-3 py-2.5"
        >
          <Search className="size-4 shrink-0 text-muted-foreground" />
          <span className="min-w-0 truncate">{query}</span>
        </CommandItem>
      ))}
    </CommandGroup>
  )
}

interface CommandAction {
  id: string
  title: string
  icon: LucideIcon
  keywords: string[]
  run: () => void
}

/**
 * One row in the flat, relevance-ranked result list used while the user is
 * typing. `category` is only a label here — it no longer decides ordering.
 */
interface RankedResult {
  key: string
  title: string
  subtitle?: string
  category: string
  icon: ReactNode
  score: number
  run: () => void
}

/**
 * The project sub-pages reachable from anywhere, without first navigating into
 * the project. Indexing every entry in `projectNavItems` for every project
 * would be projects x ~50 rows, which floods the palette and makes the Fuse
 * build noticeably slower; these are the ones worth jumping straight to.
 */
const CROSS_PROJECT_PAGE_URLS = new Set([
  'project',
  'deployments',
  'environments',
  'runtime',
  'analytics',
  'errors',
  'storage',
  'monitors',
  'metrics',
  'settings/general',
  'domains',
  'environment-variables',
  'flags',
  'git',
  'build',
])

/**
 * How well the query matches an item's *title*, on the same 0..1 scale as
 * `combinedScore`, to be added on top of the Fuse relevance.
 *
 * Fuse alone ranks an exact *keyword* hit above a near-exact *title* hit:
 * searching "team" put Users (which lists "team" as a keyword) above Teams.
 * Per-key weights don't fix it either — a zero-distance keyword match beats a
 * fuzzy title match at any weight — so the title match is scored explicitly.
 */
function titleBoost(title: string, query: string): number {
  const t = title.toLowerCase().trim()
  const q = query.toLowerCase().trim()
  if (!q || !t) return 0
  if (t === q) return 0.6
  // "team" should still find "Teams", and "teams" should find "Team".
  if (t === `${q}s` || `${t}s` === q) return 0.5
  if (t.startsWith(q)) return 0.35
  if (t.split(/\s+/).some((word) => word.startsWith(q))) return 0.25
  if (t.includes(q)) return 0.15
  return 0
}

const commandActions: CommandAction[] = [
  {
    id: 'toggle-theme',
    title: 'Toggle theme',
    icon: SunMoon,
    keywords: ['toggle', 'theme', 'dark', 'light', 'mode'],
    run: () => document.body.classList.toggle('dark'),
  },
]

const mainNavItems: NavigationItem[] = [
  {
    title: 'Dashboard',
    url: '/dashboard',
    icon: SquareTerminal,
    keywords: ['home', 'overview', 'main'],
  },
  {
    title: 'Projects',
    url: '/projects',
    icon: Folder,
    keywords: ['apps', 'applications', 'sites'],
  },
  {
    title: 'All tools',
    url: '/tools',
    icon: Boxes,
    keywords: ['tools', 'platform', 'features', 'capabilities', 'everything'],
  },
  {
    title: 'Sandboxes',
    url: '/sandboxes',
    icon: Box,
    keywords: [
      'sandbox',
      'sandboxes',
      'workspace',
      'shell',
      'terminal',
      'environment',
    ],
  },
  {
    title: 'Workspaces',
    url: '/workspaces',
    icon: Folder,
    keywords: ['workspace', 'context', 'persistent'],
  },
  {
    title: 'Deploy your first app',
    url: '/get-started',
    icon: Rocket,
    keywords: [
      'get started',
      'getting started',
      'sample',
      'demo',
      'first deploy',
      'onboarding',
      'hello',
    ],
  },
  {
    title: 'New service',
    url: '/projects/new',
    icon: FolderPlus,
    keywords: ['new', 'create', 'add', 'service', 'project', 'app', 'deploy'],
  },
  {
    // A project is a group of services; /projects opens its create dialog
    // when `new=project` is present.
    title: 'New project',
    url: '/projects?new=project',
    icon: Boxes,
    keywords: ['new', 'create', 'add', 'group', 'project', 'services'],
  },
  {
    title: 'Drop files to create a service',
    url: '/projects/new?source=drop',
    icon: Upload,
    keywords: [
      'drop',
      'upload',
      'zip',
      'folder',
      'deploy',
      'no git',
      'service',
    ],
  },
  {
    title: 'Import a service from another platform',
    url: '/projects/import-wizard',
    icon: Upload,
    keywords: [
      'import',
      'migrate',
      'workload',
      'platform',
      'external',
      'service',
      'project',
    ],
  },
  {
    title: 'Server metrics',
    url: '/monitoring/server',
    icon: Cpu,
    keywords: ['cpu', 'memory', 'disk', 'docker', 'network', 'host', 'server'],
  },
  {
    title: 'Monitoring',
    url: '/monitoring/alerts',
    icon: Gauge,
    keywords: ['metrics', 'resources', 'alerts', 'alarms', 'health'],
  },
  {
    title: 'Delivery preferences',
    url: '/monitoring/notifications',
    icon: Gauge,
    keywords: ['notification', 'preferences', 'severity', 'digest', 'email'],
  },
  {
    title: 'Proxy',
    url: '/proxy',
    icon: Activity,
    keywords: [
      'metrics',
      'performance',
      'analytics',
      'stats',
      'traffic',
      'health',
    ],
  },
  // Main navigation, not Settings: worker nodes are what builds and
  // deployments run on. The URL stays /settings/nodes (see lib/worker-nodes),
  // so this entry is filtered back out of the main category unless the page
  // is absent from the settings registry — which it now is, by design.
  {
    title: 'Worker nodes',
    url: WORKER_NODES_URL,
    icon: Network,
    keywords: [
      'worker',
      'nodes',
      'cluster',
      'multinode',
      'infrastructure',
      'build',
      'deploy',
    ],
  },
  // A card in the Worker Nodes page's collapsed Advanced section; the anchor
  // opens the section and scrolls to it.
  {
    title: 'Cluster DNS',
    url: CLUSTER_DNS_URL,
    icon: Globe,
    keywords: [
      'cluster',
      'dns',
      'temps.local',
      'service discovery',
      'resolver',
      'internal dns',
      'name resolution',
    ],
  },
]

const settingsNavItems: NavigationItem[] = [
  // General
  {
    title: 'Platform settings',
    url: '/settings',
    icon: Settings2,
    keywords: ['preferences', 'configuration', 'config', 'platform', 'general'],
  },
  {
    title: 'Notification providers',
    url: '/settings/notifications',
    icon: Bell,
    keywords: [
      'alerts',
      'notifications',
      'providers',
      'slack',
      'email',
      'webhook',
    ],
  },
  {
    title: 'Add notification provider',
    url: '/settings/notifications/new',
    icon: BellPlus,
    keywords: [
      'notifications',
      'add',
      'new',
      'slack',
      'email',
      'webhook',
      'alerts',
    ],
  },
  // Access
  {
    title: 'Users',
    url: '/settings/users',
    icon: Users,
    keywords: ['team', 'members', 'people', 'accounts'],
  },
  {
    title: 'Teams',
    url: '/settings/teams',
    icon: UsersRound,
    keywords: [
      'teams',
      'groups',
      'access',
      'permissions',
      'grants',
      'members',
      'rbac',
    ],
  },
  {
    title: 'Create team',
    url: '/settings/teams?new=1',
    icon: UsersRound,
    keywords: ['new', 'create', 'add', 'team', 'group', 'access'],
  },
  {
    title: 'Authentication',
    url: '/settings/auth',
    icon: KeyRound,
    keywords: ['sso', 'oidc', 'openid', 'identity', 'login', 'saml'],
  },
  {
    title: 'Add SSO provider',
    url: '/settings/auth/new',
    icon: KeyRound,
    keywords: ['sso', 'oidc', 'create', 'connect', 'okta', 'auth0', 'keycloak'],
  },
  {
    title: 'API keys',
    url: '/settings/keys',
    icon: Key,
    keywords: ['tokens', 'auth', 'authentication', 'api'],
  },
  {
    title: 'Create API key',
    url: '/settings/keys/new',
    icon: Key,
    keywords: ['new', 'create', 'add', 'token', 'api', 'key'],
  },
  // Infrastructure
  {
    title: 'Domains',
    url: '/domains',
    icon: Globe,
    keywords: ['dns', 'urls', 'websites', 'custom domain'],
  },
  {
    title: 'Provision domain',
    url: '/domains/add',
    icon: Globe,
    keywords: ['new', 'create', 'add', 'domain', 'dns', 'custom domain'],
  },
  {
    title: 'Databases',
    url: '/storage',
    icon: Database,
    keywords: [
      'database',
      'databases',
      'storage',
      'files',
      'data',
      'services',
      'postgres',
      'postgresql',
      'mysql',
      'redis',
      'mongodb',
      's3',
    ],
  },
  {
    title: 'Email',
    url: '/email',
    icon: Mail,
    keywords: ['email', 'mail', 'smtp', 'transactional', 'send'],
  },
  {
    title: 'AI Gateway',
    url: '/ai-gateway',
    icon: Sparkles,
    keywords: [
      'ai',
      'llm',
      'openai',
      'anthropic',
      'gateway',
      'models',
      'providers',
      'chat',
      'gpt',
      'claude',
    ],
  },
  {
    title: 'AI chat',
    url: '/chat',
    icon: MessageSquare,
    keywords: ['ai', 'chat', 'assistant', 'conversation', 'ask'],
  },
  {
    title: 'AI usage',
    url: '/ai-gateway/usage',
    icon: BarChart3,
    keywords: ['ai', 'usage', 'credits', 'cost', 'tokens', 'spend', 'billing'],
  },
  {
    title: 'AI limits',
    url: '/ai-gateway/limits',
    icon: Gauge,
    keywords: [
      'ai',
      'limits',
      'rate limit',
      'budget',
      'quota',
      'cap',
      'spend',
      '429',
    ],
  },
  {
    title: 'AI activity',
    url: '/ai-gateway/activity',
    icon: Activity,
    keywords: ['ai', 'activity', 'traces', 'telemetry', 'otel', 'spans'],
  },
  {
    title: 'AI Gateway setup',
    url: '/ai-gateway/setup',
    icon: Sparkles,
    keywords: [
      'ai',
      'setup',
      'quickstart',
      'code examples',
      'curl',
      'sdk',
      'endpoint',
      'byok',
    ],
  },
  {
    title: 'Workflows',
    url: '/ai-workflows',
    icon: Bot,
    keywords: [
      'ai',
      'ai workflows',
      'workflows',
      'agents',
      'sandbox',
      'automation',
      'autopilot',
    ],
  },
  {
    title: 'Connect AI harness',
    url: '/setup/ai',
    icon: Wand2,
    keywords: [
      'connect ai',
      'ai harness',
      'install temps skill',
      'bunx skills',
      'admin api key',
      'codex',
      'claude code',
      'cursor',
      'agent setup',
    ],
  },
  {
    title: 'Skills',
    url: '/skills',
    icon: Wand2,
    keywords: [
      'skills',
      'ai',
      'agents',
      'claude',
      'instructions',
      'prompts',
      'global',
    ],
  },
  {
    title: 'MCP servers',
    url: '/mcp-servers',
    icon: Server,
    keywords: [
      'mcp',
      'model',
      'context',
      'protocol',
      'tools',
      'servers',
      'agents',
      'claude',
      'global',
    ],
  },
  {
    title: 'Git connections',
    url: '/git-providers',
    icon: GitBranch,
    keywords: [
      'github',
      'gitlab',
      'providers',
      'version control',
      'repositories',
    ],
  },
  {
    title: 'Add Git provider',
    url: '/git-providers/add',
    icon: GitBranch,
    keywords: [
      'new',
      'create',
      'add',
      'connect',
      'github',
      'gitlab',
      'bitbucket',
      'gitea',
    ],
  },
  {
    title: 'DNS providers',
    url: '/dns-providers',
    icon: Cloud,
    keywords: [
      'dns',
      'cloudflare',
      'route53',
      'azure',
      'gcp',
      'digitalocean',
      'namecheap',
    ],
  },
  {
    title: 'Delivery profiles',
    url: '/delivery-profiles',
    icon: Cloud,
    keywords: ['delivery', 'cdn', 'cloudflare', 'direct', 'domains'],
  },
  {
    title: 'Add DNS provider',
    url: '/dns-providers/add',
    icon: Cloud,
    keywords: [
      'dns',
      'add',
      'new',
      'cloudflare',
      'route53',
      'azure',
      'gcp',
      'digitalocean',
    ],
  },
  {
    title: 'Load balancer',
    url: '/settings/load-balancer',
    icon: Server,
    keywords: ['lb', 'balancing', 'proxy', 'routes'],
  },
  {
    title: 'Docker registry',
    url: '/settings/docker-registry',
    icon: Boxes,
    keywords: ['docker', 'registry', 'container', 'image'],
  },
  {
    title: 'Build limits',
    url: '/settings/build-limits',
    icon: Gauge,
    keywords: ['build', 'limits', 'concurrency', 'resources', 'cpu', 'memory'],
  },
  {
    title: 'Backups',
    url: '/backups',
    icon: DatabaseBackup,
    keywords: [
      'restore',
      'backup',
      'backups',
      'recovery',
      's3',
      'last',
      'latest',
      'recent',
    ],
  },
  {
    title: 'Plugins',
    url: '/settings/plugins',
    icon: Puzzle,
    keywords: ['plugins', 'extensions', 'addons', 'modules'],
  },
  // Security
  {
    title: 'Security headers',
    url: '/settings/security',
    icon: Shield,
    keywords: ['security', 'headers', 'csp', 'cors', 'protection'],
  },
  {
    title: 'Shield proposals',
    url: '/settings/shield-proposals',
    icon: ShieldCheck,
    keywords: ['shield', 'learning', 'attackers', 'approve', 'block', 'review'],
  },
  {
    title: 'Rate limiting',
    url: '/settings/rate-limiting',
    icon: Monitor,
    keywords: ['rate', 'limit', 'throttle', 'ip', 'access'],
  },
  {
    title: 'Disk monitoring',
    url: '/settings/disk-monitoring',
    icon: HardDrive,
    keywords: ['disk', 'space', 'storage', 'alerts', 'monitoring'],
  },
  {
    title: 'Temps Cloud',
    url: '/settings/cloud',
    icon: Cloud,
    // "connect"/"link"/"enroll" are what the docs and the CLI call this, and
    // "backup"/"retention" are what someone is actually shopping for when they
    // go looking for it.
    keywords: [
      'cloud',
      'temps cloud',
      'managed',
      'connect',
      'link',
      'enroll',
      'telemetry',
      'retention',
      'backup',
      'subscription',
    ],
  },
  {
    title: 'Metrics monitoring',
    url: '/settings/metrics-monitoring',
    icon: BarChart3,
    keywords: [
      'metrics',
      'monitoring',
      'thresholds',
      'alerts',
      'cpu',
      'memory',
      'resources',
    ],
  },
]

const observeNavItems: NavigationItem[] = [
  {
    title: 'Proxy logs',
    url: '/proxy-logs',
    icon: Activity,
    keywords: ['logs', 'proxy', 'requests', 'traffic'],
  },
  {
    title: 'Audit logs',
    url: '/audit-logs',
    icon: ScrollText,
    keywords: ['logs', 'audit', 'history', 'activity'],
  },
]

const indexedSettingsNavItems = mergeNavigationItems(
  settingsNavItems.filter((item) => isSettingsNavigationUrl(item.url)),
  settingsPageNavigationItems
)
const secondaryNavigationUrls = new Set([
  ...observeNavItems.map((item) => item.url),
  ...indexedSettingsNavItems.map((item) => item.url),
])
export const indexedMainNavItems = excludeNavigationUrls(
  mergeNavigationItems(
    mainNavItems,
    settingsNavItems.filter((item) => !isSettingsNavigationUrl(item.url)),
    platformToolNavigationItems
  ),
  secondaryNavigationUrls
)

const accountNavItems: NavigationItem[] = [
  {
    title: 'Account',
    url: '/account',
    icon: BadgeCheck,
    keywords: ['profile', 'user', 'me'],
  },
]

// Project-specific navigation items (will be prefixed with project slug)
export const projectNavItems: NavigationItem[] = [
  {
    title: 'Service overview',
    url: 'project',
    icon: Home,
    keywords: ['home', 'overview', 'main', 'service'],
  },
  {
    title: 'Deployments',
    url: 'deployments',
    icon: GitBranch,
    keywords: ['deploy', 'releases', 'versions'],
  },
  {
    title: 'Analytics',
    url: 'analytics',
    icon: BarChart3,
    keywords: ['stats', 'metrics', 'analytics', 'overview'],
  },
  {
    title: 'Traffic filters',
    url: 'analytics/settings',
    icon: ShieldOff,
    keywords: ['bots', 'datacenter', 'cloud', 'exclude', 'filter', 'spam'],
  },
  {
    title: 'Revenue',
    url: 'revenue',
    icon: CreditCard,
    keywords: [
      'revenue',
      'mrr',
      'arr',
      'stripe',
      'billing',
      'subscriptions',
      'payments',
      'churn',
      'import',
      'csv',
    ],
  },
  {
    title: 'Visitors',
    url: 'analytics/visitors',
    icon: Users,
    keywords: ['users', 'visitors', 'traffic', 'analytics'],
  },
  {
    title: 'Live visitors',
    url: 'analytics/live-visitors',
    icon: Users,
    keywords: ['live', 'realtime', 'online', 'active', 'now', 'analytics'],
  },
  {
    title: 'Live globe',
    url: 'analytics/live',
    icon: Globe,
    keywords: ['live', 'globe', 'map', 'realtime', 'world', 'analytics'],
  },
  {
    title: 'Journey',
    url: 'analytics/journey',
    icon: Activity,
    keywords: ['journey', 'flow', 'paths', 'navigation', 'analytics'],
  },
  {
    title: 'Pages',
    url: 'analytics/pages',
    icon: Activity,
    keywords: ['pages', 'views', 'pageviews', 'analytics'],
  },
  {
    title: 'AI agents',
    url: 'analytics/ai-agents',
    icon: Bot,
    keywords: ['ai', 'agents', 'bots', 'llm', 'analytics', 'traffic'],
  },
  {
    title: 'Session replays',
    url: 'analytics/replays',
    icon: Monitor,
    keywords: ['session', 'replays', 'recordings', 'analytics'],
  },
  {
    title: 'Funnels',
    url: 'analytics/funnels',
    icon: BarChart3,
    keywords: ['funnels', 'conversion', 'flow', 'analytics'],
  },
  {
    title: 'Analytics setup',
    url: 'analytics/setup',
    icon: Settings,
    keywords: ['setup', 'configuration', 'install', 'analytics'],
  },
  {
    title: 'API traffic',
    url: 'analytics/api-traffic',
    icon: Server,
    keywords: ['api', 'traffic', 'requests', 'analytics'],
  },
  {
    title: 'Databases',
    url: 'storage',
    icon: Database,
    keywords: ['database', 'databases', 'storage', 'data'],
  },
  {
    title: 'Logs',
    url: 'runtime',
    icon: ScrollText,
    keywords: ['logs', 'runtime', 'console', 'output', 'live'],
  },
  {
    title: 'Log history',
    url: 'runtime?tab=history',
    icon: History,
    keywords: ['logs', 'history', 'search', 'archive', 'past'],
  },
  {
    title: 'Speed insights',
    url: 'speed',
    icon: Monitor,
    keywords: ['performance', 'speed', 'insights', 'vitals'],
  },
  {
    title: 'Error tracking',
    url: 'errors',
    icon: Shield,
    keywords: ['errors', 'exceptions', 'bugs', 'tracking'],
  },
  {
    title: 'Uptime',
    url: 'monitors',
    icon: Activity,
    keywords: ['monitoring', 'uptime', 'health', 'monitors'],
  },
  {
    title: 'Traces',
    url: 'traces',
    icon: Workflow,
    keywords: [
      'traces',
      'opentelemetry',
      'otel',
      'spans',
      'tracing',
      'distributed',
    ],
  },
  {
    title: 'AI crawlers',
    url: 'ai-crawlers',
    icon: Bot,
    keywords: [
      'ai',
      'crawlers',
      'bots',
      'gptbot',
      'googlebot',
      'scrapers',
      'observe',
    ],
  },
  {
    title: 'Service settings',
    url: 'settings/general',
    icon: Settings,
    keywords: ['settings', 'configuration', 'general', 'service'],
  },
  {
    title: 'Feature flags',
    url: settingsSectionPath('delivery', 'feature-flags'),
    icon: Flag,
    keywords: ['flags', 'feature flags', 'toggles', 'rollout'],
  },
  {
    title: 'Domains',
    url: 'domains',
    icon: Globe,
    keywords: ['domains', 'dns', 'custom domain'],
  },
  {
    title: 'Environments',
    url: 'environments',
    icon: Database,
    keywords: ['environments', 'env', 'staging', 'production'],
  },
  {
    title: 'Environment variables',
    url: 'environment-variables',
    icon: Key,
    keywords: ['variables', 'env', 'config'],
  },
  {
    title: 'Secrets',
    url: settingsSectionPath('variables', 'secrets'),
    icon: FileLock2,
    keywords: ['secrets', 'secret files', 'mounted secrets', '/run/secrets'],
  },
  {
    title: 'Deployment tokens',
    url: settingsSectionPath('deployment-tokens', 'deployment-tokens'),
    icon: Key,
    keywords: ['tokens', 'deployment tokens', 'TEMPS_API_TOKEN', 'api'],
  },
  {
    title: 'Git',
    url: settingsSectionPath('delivery', 'repository'),
    icon: GitBranch,
    keywords: ['git', 'repository', 'repo', 'source'],
  },
  {
    title: 'Build & deploy',
    url: 'settings/delivery',
    icon: Settings,
    keywords: ['build', 'framework', 'compose', 'docker', 'root directory'],
  },
  {
    title: 'Security',
    url: 'settings/security',
    icon: Shield,
    keywords: ['security', 'headers', 'rate limiting', 'protection'],
  },
  {
    title: 'Vulnerability scanning',
    url: 'settings/security#vulnerability-scanning',
    icon: ShieldCheck,
    keywords: ['scan', 'scans', 'cve', 'trivy', 'vulnerabilities', 'image'],
  },
  {
    title: 'Security headers',
    url: 'settings/security#security-headers',
    icon: Shield,
    keywords: ['headers', 'csp', 'hsts', 'content security policy'],
  },
  {
    title: 'Rate limiting',
    url: 'settings/security#rate-limiting',
    icon: Gauge,
    keywords: ['rate limit', 'throttle', 'requests per minute'],
  },
  {
    title: 'Shield',
    url: 'settings/security#shield',
    icon: Shield,
    keywords: ['shield', 'waf', 'bots', 'firewall', 'block'],
  },
  {
    title: 'Access',
    url: 'settings/access',
    icon: Users,
    keywords: ['access', 'permissions', 'members', 'roles', 'team'],
  },
  {
    title: 'Cron jobs',
    url: 'cron-jobs',
    icon: Activity,
    keywords: [
      'cron',
      'jobs',
      'scheduled',
      'tasks',
      '.temps.yaml',
      'temps.yaml',
    ],
  },
  {
    title: 'Check-ins',
    url: 'check-ins',
    icon: Activity,
    keywords: [
      'check-in',
      'cron monitor',
      'heartbeat',
      'missed job',
      'sentry',
      'withMonitor',
    ],
  },
  {
    title: 'Webhooks',
    url: settingsSectionPath('integrations', 'webhooks'),
    icon: Workflow,
    keywords: ['webhooks', 'hooks', 'events', 'callbacks', 'integrations'],
  },
  {
    title: 'Service skills',
    url: settingsSectionPath('integrations', 'skills'),
    icon: Wand2,
    keywords: [
      'skills',
      'ai',
      'agents',
      'claude',
      'instructions',
      'service',
      'project',
    ],
  },
  {
    title: 'Service MCP servers',
    url: settingsSectionPath('integrations', 'mcp-servers'),
    icon: Server,
    keywords: [
      'mcp',
      'model',
      'context',
      'protocol',
      'tools',
      'servers',
      'service',
      'project',
    ],
  },
  {
    title: 'Metrics',
    url: 'metrics',
    icon: BarChart3,
    keywords: [
      'metrics',
      'opentelemetry',
      'otel',
      'cpu',
      'memory',
      'resources',
      'observe',
    ],
  },
  {
    title: 'Activity',
    url: 'observe',
    icon: Activity,
    keywords: [
      'observe',
      'events',
      'opentelemetry',
      'otel',
      'timeline',
      'all events',
    ],
  },
  {
    title: 'Telemetry logs',
    url: 'telemetry-logs',
    icon: ScrollText,
    keywords: ['logs', 'opentelemetry', 'otel', 'observe'],
  },
  {
    title: 'Services',
    url: 'services',
    icon: Boxes,
    keywords: ['services', 'kv', 'blob', 'storage', 'redis', 's3'],
  },
  {
    title: 'Services - KV store',
    url: 'services/kv',
    icon: Database,
    keywords: ['kv', 'key-value', 'redis', 'cache', 'storage'],
  },
  {
    title: 'Services - Blob storage',
    url: 'services/blob',
    icon: HardDrive,
    keywords: ['blob', 's3', 'files', 'storage', 'uploads', 'objects'],
  },
  {
    title: 'AI traces',
    url: 'ai-gateway',
    icon: Bot,
    keywords: [
      'ai',
      'traces',
      'observability',
      'llm',
      'openai',
      'anthropic',
      'models',
      'gateway',
      'otel',
      'gen_ai',
    ],
  },
  {
    title: 'Automations',
    url: settingsSectionPath('automation', 'automations'),
    icon: Bot,
    keywords: [
      'automation',
      'agents',
      'autopilot',
      'ai',
      'chat',
      'workflows',
      'prompt',
      'schedule',
    ],
  },
  {
    title: 'Crash autofix',
    url: settingsSectionPath('automation', 'crash-autofix'),
    icon: Wand2,
    keywords: [
      'autofix',
      'autofixer',
      'crash',
      'errors',
      'fix',
      'repair',
      'incident',
      'confidence',
    ],
  },
  {
    title: 'Error alert rules',
    url: 'errors/alert-rules',
    icon: Bell,
    keywords: ['errors', 'alerts', 'rules', 'notifications'],
  },
  {
    title: 'Security scans',
    url: 'security',
    icon: Shield,
    keywords: ['security', 'scans', 'vulnerabilities', 'cve'],
  },
  {
    title: 'Request logs',
    url: 'request-logs',
    icon: Network,
    keywords: ['logs', 'requests', 'http', 'traffic'],
  },
  // Single settings inside the settings pages ("Docker Compose", "Health
  // checks"), each opening its page scrolled to that setting.
  ...PROJECT_SETTINGS_INDEX.map((entry) => ({
    title: entry.title,
    url: projectSettingUrl(entry),
    icon: Settings2,
    keywords: ['setting', 'settings', ...entry.keywords],
    preset: entry.preset,
  })),
]

/**
 * The project pages and settings `project` actually has: a monitoring-only
 * (external) project registers no hosting pages, so offering them would land
 * on "Add hosting" or, for settings, on the wrong page; and a setting specific
 * to one build preset (the Compose file) is not offered to services built
 * another way.
 */
export function projectNavItemsFor(project: {
  source_type?: string | null
  preset?: string | null
}): NavigationItem[] {
  return projectNavItems.filter(
    (item) =>
      projectHasPage(project, item.url) && projectHasSetting(project, item)
  )
}

export function CommandPalette() {
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState('')
  // The highlighted row, controlled so it always matches the ranked list.
  const [activeValue, setActiveValue] = useState('')
  const navigate = useNavigate()
  const location = useLocation()
  const { platformNavEntries, settingsNavEntries, projectNavEntries } =
    usePluginsContext()
  const showFullBrowseCatalog =
    localStorage.getItem('temps:show-full-command-catalog') === 'true'

  const {
    data: projectResponse,
    refetch: refetchProjects,
    fetchNextPage: fetchNextProjectPage,
    hasNextPage: hasNextProjectPage,
    isFetchingNextPage: isFetchingNextProjectPage,
  } = useInfiniteQuery({
    ...getProjectsInfiniteOptions({ query: { per_page: 100 } }),
    initialPageParam: 1,
    getNextPageParam: (lastPage) => {
      const loaded = lastPage.page * lastPage.per_page
      return loaded < lastPage.total ? lastPage.page + 1 : undefined
    },
    enabled: open,
  })
  const projects = useMemo(
    () => projectResponse?.pages.flatMap((page) => page.projects) ?? [],
    [projectResponse]
  )

  // Project groups ("Projects" in the console). Same query options as
  // useProjectGroups so the cache is shared, but only fetched while the
  // palette is open, like every other list here.
  const { data: groupsData, refetch: refetchGroups } = useQuery({
    ...listProjectGroupsOptions({ query: { page_size: GROUP_PAGE_SIZE } }),
    enabled: open,
  })
  const groups = useMemo(() => groupsData?.groups ?? [], [groupsData])

  const { data: globalSkillsData, refetch: refetchSkills } = useQuery({
    ...listGlobalSkillsOptions(),
    enabled: open,
    staleTime: 60_000,
  })
  const globalSkills = useMemo(
    () => globalSkillsData?.items ?? [],
    [globalSkillsData]
  )

  const { data: globalMcpServersData, refetch: refetchMcp } = useQuery({
    ...listGlobalMcpsOptions(),
    enabled: open,
    staleTime: 60_000,
  })
  const globalMcpServers = useMemo(
    () => globalMcpServersData?.items ?? [],
    [globalMcpServersData]
  )

  const {
    data: serviceResponse,
    refetch: refetchServices,
    fetchNextPage: fetchNextServicePage,
    hasNextPage: hasNextServicePage,
    isFetchingNextPage: isFetchingNextServicePage,
  } = useInfiniteQuery({
    ...listServicesInfiniteOptions({ query: { page_size: 100 } }),
    initialPageParam: 1,
    getNextPageParam: (lastPage, pages) =>
      lastPage.length === 100 ? pages.length + 1 : undefined,
    enabled: open,
    staleTime: 60_000,
  })
  const services = useMemo(
    () => serviceResponse?.pages.flatMap((page) => page) ?? [],
    [serviceResponse]
  )

  useEffect(() => {
    if (open && hasNextProjectPage && !isFetchingNextProjectPage) {
      void fetchNextProjectPage()
    }
  }, [
    open,
    hasNextProjectPage,
    isFetchingNextProjectPage,
    fetchNextProjectPage,
  ])

  useEffect(() => {
    if (open && hasNextServicePage && !isFetchingNextServicePage) {
      void fetchNextServicePage()
    }
  }, [
    open,
    hasNextServicePage,
    isFetchingNextServicePage,
    fetchNextServicePage,
  ])

  // Detect if user is on a project page and extract slug
  const currentProjectSlug = useMemo(() => {
    const match = location.pathname.match(/^\/projects\/([^/]+)/)
    return match ? match[1] : null
  }, [location.pathname])

  const currentProject = useMemo(() => {
    if (!currentProjectSlug) return null
    return projects.find((p) => p.slug === currentProjectSlug)
  }, [currentProjectSlug, projects])

  const sampleQueries = useMemo(() => {
    const orderedProjects = currentProject
      ? [currentProject, ...projects.filter((p) => p.id !== currentProject.id)]
      : projects
    return buildCommandSampleQueries({
      projectSlugs: orderedProjects.map((project) => project.slug),
      services: services.map((service) => ({
        name: service.name,
        serviceType: service.service_type,
      })),
    })
  }, [currentProject, projects, services])
  // Refetch when the dialog is opened or when react-query invalidates
  useEffect(() => {
    if (open) {
      refetchProjects()
      refetchGroups()
      refetchSkills()
      refetchMcp()
      refetchServices()
    }
  }, [
    open,
    refetchProjects,
    refetchGroups,
    refetchSkills,
    refetchMcp,
    refetchServices,
  ])

  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      if (e.key === 'k' && (e.metaKey || e.ctrlKey)) {
        e.preventDefault()
        setOpen((open) => !open)
      }
    }
    document.addEventListener('keydown', down)
    return () => document.removeEventListener('keydown', down)
  }, [])

  const { record, getScore, recent } = useFrecency()

  /**
   * Rank one candidate. The title match dominates; the Fuse score — which is
   * also what makes keyword/alias hits findable at all — and frecency only
   * break ties.
   *
   * The weights are the point: a keyword-only hit scores at most 0.25, while
   * any title hit starts at 0.15 and an exact one reaches 0.60. So a tag still
   * ranks (searching "team" finds Users, which is tagged `team`) but never
   * above the page actually called Teams.
   */
  const rank = useCallback(
    (key: string, title: string, fuseScore: number | undefined, damp = 1) =>
      titleBoost(title, search) * damp +
      // Fuse score: 0 = perfect match, 1 = no match. Invert to relevance.
      (1 - (fuseScore ?? 0)) * 0.25 +
      normalizeFrecency(getScore(key)) * 0.15,
    [search, getScore]
  )

  const runCommand = (command: () => void) => {
    setOpen(false)
    setSearch('')
    command()
  }

  const runWithFrecency = (key: string, command: () => void) => {
    record(key)
    runCommand(command)
  }

  // Build plugin navigation items for the command palette.
  //
  // These come from the context's *resolved* entries, not from
  // `plugins[].nav` — a manifest's own path (`/builder`) matches no console
  // route, so mapping the manifests here sent every plugin command to a 404
  // while the sidebar, which uses the resolved entries, worked.
  const pluginNavItems: NavigationItem[] = useMemo(
    () =>
      [...platformNavEntries, ...settingsNavEntries].map((entry) => ({
        title: entry.label,
        url: entry.path,
        icon: resolvePluginIcon(entry.icon),
        keywords: ['plugin', entry.pluginName, entry.label.toLowerCase()],
      })),
    [platformNavEntries, settingsNavEntries]
  )

  // Project-scoped plugin nav entries (relative URLs, prefixed at render time)
  const projectPluginNavItems: NavigationItem[] = useMemo(
    () =>
      projectNavEntries.map((entry) => ({
        title: entry.label,
        url: entry.path,
        icon: resolvePluginIcon(entry.icon),
        keywords: ['plugin', 'service', 'project', entry.label.toLowerCase()],
      })),
    [projectNavEntries]
  )

  // Settings links contributed by a console extension. Indexed at runtime,
  // unlike the static registry above, because extensions arrive via
  // context rather than at module load.
  const { settingsNavItems: extensionSettingsItems } = useConsoleExtensions()
  const extensionSettingsNavItems: NavigationItem[] = useMemo(
    () =>
      indexSettingsNavigationGroups(
        mergeSettingsNavigationGroups([], extensionSettingsItems)
      ),
    [extensionSettingsItems]
  )

  const canViewAuditLogs = useCanViewAuditLogs()
  const visibleObserveNavItems = useMemo(
    () => filterRestrictedNavigationItems(observeNavItems, canViewAuditLogs),
    [canViewAuditLogs]
  )

  // Create Fuse instances for fuzzy search
  const navFuse = useMemo(() => {
    const allNavItems = [
      ...indexedMainNavItems.map((item) => ({
        ...item,
        category: 'Navigation',
      })),
      ...indexedSettingsNavItems.map((item) => ({
        ...item,
        category: 'Settings',
      })),
      ...extensionSettingsNavItems.map((item) => ({
        ...item,
        category: 'Settings',
      })),
      ...visibleObserveNavItems.map((item) => ({
        ...item,
        category: 'Observe',
      })),
      ...accountNavItems.map((item) => ({ ...item, category: 'Account' })),
      ...pluginNavItems.map((item) => ({ ...item, category: 'Plugins' })),
    ]

    // Add project-specific navigation if we're on a project page
    if (currentProjectSlug && currentProject) {
      const projectSpecificItems = [
        ...projectNavItemsFor(currentProject),
        ...projectPluginNavItems,
      ].map((item) => ({
        ...item,
        // Prepend project slug to URL for absolute navigation
        url: `/projects/${currentProjectSlug}/${item.url}`,
        category: TERMS.service.singular,
      }))
      allNavItems.push(...projectSpecificItems)
    }

    return new Fuse(allNavItems, NAV_FUSE_OPTIONS)
  }, [
    currentProjectSlug,
    currentProject,
    pluginNavItems,
    projectPluginNavItems,
    visibleObserveNavItems,
    extensionSettingsNavItems,
  ])

  const projectsFuse = useMemo(() => {
    return new Fuse(projects, {
      keys: [
        { name: 'name', weight: 2 },
        { name: 'slug', weight: 1 },
      ],
      threshold: 0.3,
      includeScore: true,
      shouldSort: true,
      minMatchCharLength: 1,
    })
  }, [projects])

  const groupsFuse = useMemo(() => {
    return new Fuse(groups, {
      keys: [
        { name: 'name', weight: 2 },
        { name: 'slug', weight: 1 },
      ],
      threshold: 0.3,
      includeScore: true,
      shouldSort: true,
      minMatchCharLength: 1,
    })
  }, [groups])

  const skillsFuse = useMemo(() => {
    return new Fuse(globalSkills, {
      keys: [
        { name: 'name', weight: 2 },
        { name: 'slug', weight: 1.5 },
        { name: 'description', weight: 1 },
      ],
      threshold: 0.3,
      includeScore: true,
      shouldSort: true,
      minMatchCharLength: 1,
    })
  }, [globalSkills])

  // Every project x a bounded set of its pages, so the palette can jump
  // straight to "<project> Deployments" from anywhere instead of only offering
  // project sub-pages once you are already inside that project.
  const crossProjectItems = useMemo(() => {
    const pages = projectNavItems.filter((item) =>
      CROSS_PROJECT_PAGE_URLS.has(item.url)
    )
    return projects.flatMap((project) =>
      pages
        .filter((page) => projectHasPage(project, page.url))
        .map((page) => ({
          title: page.title,
          projectName: project.name,
          url: `/projects/${project.slug}/${page.url}`,
          icon: page.icon,
          // One field so a two-word query ("demo deploy") can match the project
          // and the page at once.
          searchText: `${project.name} ${project.slug} ${page.title} ${(
            page.keywords ?? []
          ).join(' ')}`,
        }))
    )
  }, [projects])

  const crossProjectFuse = useMemo(() => {
    return new Fuse(crossProjectItems, {
      keys: ['searchText'],
      // Extended search so each whitespace-separated token must match
      // ("demo deploy" = 'demo AND 'deploy). Plain fuzzy treats the query as
      // one contiguous pattern and would miss "<project> <page>".
      useExtendedSearch: true,
      threshold: 0.3,
      includeScore: true,
      shouldSort: true,
      minMatchCharLength: 1,
    })
  }, [crossProjectItems])

  const mcpFuse = useMemo(() => {
    return new Fuse(globalMcpServers, {
      keys: [
        { name: 'name', weight: 2 },
        { name: 'slug', weight: 1.5 },
        { name: 'description', weight: 1 },
      ],
      threshold: 0.3,
      includeScore: true,
      shouldSort: true,
      minMatchCharLength: 1,
    })
  }, [globalMcpServers])

  // Browse mode: the full, sectioned lists shown when the input is empty.
  // Once the user types, `rankedResults` takes over — see the comment there.
  const browseResults = useMemo(() => {
    const projectNavigation =
      currentProjectSlug && currentProject
        ? [...projectNavItemsFor(currentProject), ...projectPluginNavItems].map(
            (item) => ({
              ...item,
              url: `/projects/${currentProjectSlug}/${item.url}`,
            })
          )
        : []

    return {
      navigation: indexedMainNavItems,
      settings: [...indexedSettingsNavItems, ...extensionSettingsNavItems],
      observe: visibleObserveNavItems,
      account: accountNavItems,
      plugins: pluginNavItems,
      projectNav: projectNavigation,
      groups,
      projects,
      skills: globalSkills,
      mcpServers: globalMcpServers,
      actions: commandActions,
    }
  }, [
    groups,
    projects,
    globalSkills,
    globalMcpServers,
    pluginNavItems,
    projectPluginNavItems,
    currentProjectSlug,
    currentProject,
    visibleObserveNavItems,
    extensionSettingsNavItems,
  ])

  const commandDestinations = useMemo(() => {
    const destination = (
      id: string,
      item: NavigationItem,
      category: string,
      description: string = `Open ${item.title}`
    ): CommandDestination => ({
      id,
      title: item.title,
      description,
      url: item.url,
      category,
      keywords: item.keywords ?? [],
    })

    const instancePages = [
      ...browseResults.navigation.map((item) =>
        destination(`instance:${item.url}`, item, 'Temps')
      ),
      ...browseResults.settings.map((item) =>
        destination(`settings:${item.url}`, item, 'Instance settings')
      ),
      ...browseResults.observe.map((item) =>
        destination(`observe:${item.url}`, item, 'Observe')
      ),
      ...browseResults.account.map((item) =>
        destination(`account:${item.url}`, item, 'Account')
      ),
      ...browseResults.plugins.map((item) =>
        destination(`plugin:${item.url}`, item, 'Plugin')
      ),
    ]

    // Put the active project first as a weak default for requests that do not
    // name one. Every project's route still includes its slug and name, so an
    // explicit slug remains unambiguous from anywhere in the console.
    const orderedProjects = currentProject
      ? [currentProject, ...projects.filter((p) => p.id !== currentProject.id)]
      : projects
    const projectPages = orderedProjects.flatMap((project) => {
      const category =
        project.id === currentProject?.id
          ? `Current service · ${project.slug}`
          : `${TERMS.service.singular} · ${project.slug}`
      const pages = projectNavItemsFor(project).map((item) => {
        const url = `/projects/${project.slug}/${item.url}`
        return destination(
          `project:${project.slug}:${item.url}`,
          {
            ...item,
            title: `${item.title} · ${project.slug}`,
            url,
            keywords: [
              ...(item.keywords ?? []),
              project.name,
              project.slug,
              project.id === currentProject?.id ? 'current service' : '',
              project.id === currentProject?.id ? 'current project' : '',
            ].filter(Boolean),
          },
          category,
          `Open ${item.title} for service ${project.name} (${project.slug})`
        )
      })

      pages.push({
        id: `project:${project.slug}:environment:production`,
        title: `Production environment · ${project.slug}`,
        description: `Open the production environment for service ${project.name} (${project.slug})`,
        url: `/projects/${project.slug}/environments?environment=production`,
        category,
        keywords: [
          'environment',
          'production',
          'live',
          project.name,
          project.slug,
        ],
      })
      return pages
    })

    const groupPages: CommandDestination[] = groups.map((group) => ({
      id: `group:${group.id}`,
      title: group.name,
      description: `Open project ${group.name} (${group.slug})`,
      url: groupHref(group.slug),
      category: TERMS.group.singular,
      keywords: ['project', 'group', 'services', group.name, group.slug],
    }))

    const servicePages: CommandDestination[] = services.map((service) => ({
      id: `service:${service.id}`,
      title: `${service.name} · ${service.service_type}`,
      description: `Open the ${service.service_type} database named ${service.name}`,
      url: `/storage/${service.id}`,
      // Not TERMS.service: these are managed databases/storage, listed under
      // Databases, and must not read like the deployable services above.
      category: 'Database',
      keywords: [
        'service',
        'database',
        'storage',
        service.name,
        service.service_type,
      ],
    }))

    const skillPages: CommandDestination[] = globalSkills.map((skill) => ({
      id: `skill:${skill.slug}`,
      title: skill.name,
      description: skill.description || `Open skill ${skill.name}`,
      url: `/skills/${skill.slug}`,
      category: 'Skill',
      keywords: ['skill', skill.name, skill.slug],
    }))
    const mcpPages: CommandDestination[] = globalMcpServers.map((mcp) => ({
      id: `mcp:${mcp.slug}`,
      title: mcp.name,
      description: mcp.description || `Open MCP server ${mcp.name}`,
      url: `/mcp-servers/${mcp.slug}`,
      category: 'MCP Server',
      keywords: ['mcp', 'server', mcp.name, mcp.slug],
    }))

    return dedupeCommandDestinations([
      ...instancePages,
      ...groupPages,
      ...projectPages,
      ...servicePages,
      ...skillPages,
      ...mcpPages,
    ])
  }, [
    browseResults,
    currentProject,
    groups,
    projects,
    services,
    globalSkills,
    globalMcpServers,
  ])

  const commandDestinationFuse = useMemo(
    () =>
      new Fuse(
        commandDestinations.map((destination) => ({
          ...destination,
          searchText: [
            destination.title,
            destination.description,
            destination.category,
            ...destination.keywords,
          ].join(' '),
        })),
        {
          keys: ['searchText'],
          threshold: 0.35,
          includeScore: true,
          shouldSort: true,
          ignoreLocation: true,
          useExtendedSearch: true,
        }
      ),
    [commandDestinations]
  )

  // Search mode: ONE list ordered by relevance (blended with frecency).
  //
  // This used to be grouped by section and each section rendered in a fixed
  // order, so a weak keyword hit in "Navigation" beat an exact title match in
  // "Settings" purely because Navigation renders first — searching "work" put
  // Sandboxes (keyword: workspace) above Worker Nodes. Section is a label
  // here, not an ordering.
  const rankedResults = useMemo<RankedResult[]>(() => {
    if (!search) return []
    const out: RankedResult[] = []
    for (const result of navFuse.search(search)) {
      const item = result.item
      const Icon = item.icon
      out.push({
        key: item.url,
        title: item.title,
        subtitle:
          item.category === TERMS.service.singular && currentProject
            ? currentProject.name
            : undefined,
        category: item.category,
        icon: <Icon className="h-4 w-4" />,
        score: rank(item.url, item.title, result.score),
        run: () => navigate(item.url),
      })
    }

    for (const result of groupsFuse.search(search)) {
      const group = result.item
      out.push({
        key: `group:${group.id}`,
        title: group.name,
        subtitle: group.slug,
        category: TERMS.group.singular,
        icon: <Boxes className="h-4 w-4" />,
        score: rank(`group:${group.id}`, group.name, result.score),
        run: () => navigate(groupHref(group.slug)),
      })
    }

    for (const result of projectsFuse.search(search)) {
      const project = result.item
      out.push({
        key: `project:${project.id}`,
        title: project.slug,
        category: TERMS.service.singular,
        icon: <ProjectAvatar name={project.name} className="size-5" />,
        score: rank(`project:${project.id}`, project.name, result.score),
        run: () => navigate(`/projects/${project.slug}`),
      })
    }

    const tokens = search.trim().split(/\s+/).filter(Boolean)
    if (tokens.length > 0) {
      const extendedQuery = tokens.map((token) => `'${token}`).join(' ')
      for (const result of crossProjectFuse.search(extendedQuery)) {
        const item = result.item
        const Icon = item.icon
        out.push({
          key: item.url,
          title: item.title,
          subtitle: item.projectName,
          category: TERMS.service.singular,
          icon: <Icon className="h-4 w-4" />,
          // Damped: a top-level page named X should beat every project's X.
          score: rank(item.url, item.title, result.score, 0.6),
          run: () => navigate(item.url),
        })
      }
    }

    for (const result of skillsFuse.search(search)) {
      const skill = result.item
      out.push({
        key: `skill:${skill.slug}`,
        title: skill.name,
        subtitle: skill.slug,
        category: 'Skill',
        icon: <Wand2 className="h-4 w-4" />,
        score: rank(`skill:${skill.slug}`, skill.name, result.score),
        run: () => navigate(`/skills/${skill.slug}`),
      })
    }

    for (const result of mcpFuse.search(search)) {
      const mcp = result.item
      out.push({
        key: `mcp:${mcp.slug}`,
        title: mcp.name,
        subtitle: mcp.slug,
        category: 'MCP Server',
        icon: <Server className="h-4 w-4" />,
        score: rank(`mcp:${mcp.slug}`, mcp.name, result.score),
        run: () => navigate(`/mcp-servers/${mcp.slug}`),
      })
    }

    for (const action of commandActions) {
      const actionFuse = new Fuse([action.title, ...action.keywords], {
        threshold: 0.4,
        includeScore: true,
      })
      const hit = actionFuse.search(search)[0]
      if (!hit) continue
      const Icon = action.icon
      out.push({
        key: `action:${action.id}`,
        title: action.title,
        category: 'Action',
        icon: <Icon className="h-4 w-4" />,
        score: rank(`action:${action.id}`, action.title, hit.score),
        run: action.run,
      })
    }

    // The current project's pages are indexed by both navFuse and the
    // cross-project index; sorting first means the dedupe keeps the better
    // scoring copy.
    const seen = new Set<string>()
    return out
      .sort((a, b) => b.score - a.score)
      .filter((entry) => {
        if (seen.has(entry.key)) return false
        seen.add(entry.key)
        return true
      })
  }, [
    search,
    navFuse,
    groupsFuse,
    projectsFuse,
    crossProjectFuse,
    skillsFuse,
    mcpFuse,
    currentProject,
    rank,
    navigate,
  ])

  const localMatches = useMemo(() => {
    const query = toCommandExtendedQuery(search)
    return query ? commandDestinationFuse.search(query).slice(0, 8) : []
  }, [commandDestinationFuse, search])

  const constrainedDestination = useMemo(() => {
    const query = search.trim()
    if (!query) return undefined

    const projectSlugs = projects.map((project) => project.slug)
    const explicitProjectEnvironment = resolveExplicitProjectEnvironment(
      query,
      projectSlugs,
      commandDestinations,
      currentProjectSlug
    )
    if (explicitProjectEnvironment) return explicitProjectEnvironment

    return resolveExplicitNamedProjectDestination(
      query,
      projectSlugs,
      localMatches.map(({ item }) => item)
    )
  }, [search, projects, commandDestinations, currentProjectSlug, localMatches])

  // What the list renders IS what Enter opens. The explicit resolvers above
  // used to run only on the Enter key, so a query they answered navigated to a
  // row that was never highlighted (and often never even first).
  const visibleResults = useMemo<RankedResult[]>(() => {
    if (!constrainedDestination) return rankedResults
    const existing = rankedResults.find(
      (result) => result.key === constrainedDestination.url
    )
    return hoistResultFirst(
      rankedResults,
      existing ?? {
        key: constrainedDestination.url,
        title: constrainedDestination.title,
        category: constrainedDestination.category,
        icon: <Search className="h-4 w-4" />,
        score: Number.POSITIVE_INFINITY,
        run: () => navigate(constrainedDestination.url),
      }
    )
  }, [rankedResults, constrainedDestination, navigate])

  const visibleLocalMatches = useMemo(
    () =>
      localMatches
        .filter(
          ({ item }) =>
            !visibleResults.some((result) => result.key === item.url)
        )
        .slice(0, 8),
    [localMatches, visibleResults]
  )

  // cmdk owns the highlighted row, but with `shouldFilter={false}` it only
  // re-selects the first item on a scheduled pass that can land before this
  // re-ranked list has rendered — leaving the highlight (and therefore the
  // Enter target) on a stale row. Driving `value` ourselves keeps the two in
  // sync on every keystroke.
  const firstResultValue = search
    ? (visibleResults[0]?.key ??
      (visibleLocalMatches[0]
        ? `local-${visibleLocalMatches[0].item.id}`
        : undefined))
    : undefined
  useEffect(() => {
    // cmdk is the external widget being synchronised here: its highlight has
    // to be pushed back to the head of the list whenever re-ranking moves it.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setActiveValue(firstResultValue ?? '')
  }, [firstResultValue])

  // Resolve recent frecency keys into renderable items (icon + title + run).
  interface RecentEntry {
    key: string
    title: string
    subtitle?: string
    icon: ReactNode
    run: () => void
  }
  const recentItems = useMemo<RecentEntry[]>(() => {
    if (search) return []
    const allNavItems: NavigationItem[] = [
      ...indexedMainNavItems,
      ...indexedSettingsNavItems,
      ...extensionSettingsNavItems,
      ...visibleObserveNavItems,
      ...accountNavItems,
      ...pluginNavItems,
    ]
    const projectNavigation =
      currentProjectSlug && currentProject
        ? [...projectNavItemsFor(currentProject), ...projectPluginNavItems].map(
            (item) => ({
              ...item,
              url: `/projects/${currentProjectSlug}/${item.url}`,
            })
          )
        : []
    const navByUrl = buildAccessibleNavigationMap(
      [...allNavItems, ...projectNavigation],
      canViewAuditLogs
    )
    const groupsById = new Map(groups.map((g) => [String(g.id), g]))
    const projectsById = new Map(projects.map((p) => [String(p.id), p]))
    const skillsBySlug = new Map(globalSkills.map((s) => [s.slug, s]))
    const mcpBySlug = new Map(globalMcpServers.map((m) => [m.slug, m]))

    const out: RecentEntry[] = []
    for (const key of recent(7)) {
      if (key.startsWith('group:')) {
        const group = groupsById.get(key.slice('group:'.length))
        if (!group) continue
        out.push({
          key,
          title: group.name,
          subtitle: group.slug,
          icon: <Boxes className="h-4 w-4" />,
          run: () => navigate(groupHref(group.slug)),
        })
      } else if (key.startsWith('project:')) {
        const project = projectsById.get(key.slice('project:'.length))
        if (!project) continue
        out.push({
          key,
          title: project.slug,
          icon: <ProjectAvatar name={project.name} className="size-5" />,
          run: () => navigate(`/projects/${project.slug}`),
        })
      } else if (key.startsWith('skill:')) {
        const skill = skillsBySlug.get(key.slice('skill:'.length))
        if (!skill) continue
        out.push({
          key,
          title: skill.name,
          subtitle: skill.slug,
          icon: <Wand2 className="h-4 w-4" />,
          run: () => navigate(`/skills/${skill.slug}`),
        })
      } else if (key.startsWith('mcp:')) {
        const mcp = mcpBySlug.get(key.slice('mcp:'.length))
        if (!mcp) continue
        out.push({
          key,
          title: mcp.name,
          subtitle: mcp.slug,
          icon: <Server className="h-4 w-4" />,
          run: () => navigate(`/mcp-servers/${mcp.slug}`),
        })
      } else if (key.startsWith('action:')) {
        const action = commandActions.find(
          ({ id }) => id === key.slice('action:'.length)
        )
        if (!action) continue
        const Icon = action.icon
        out.push({
          key,
          title: action.title,
          icon: <Icon className="h-4 w-4" />,
          run: action.run,
        })
      } else {
        // Treat as nav URL
        const nav = navByUrl.get(key)
        if (!nav) continue
        const Icon = nav.icon
        out.push({
          key,
          title: nav.title,
          icon: <Icon className="h-4 w-4" />,
          run: () => navigate(nav.url),
        })
      }
    }
    return out
  }, [
    search,
    recent,
    pluginNavItems,
    projectPluginNavItems,
    currentProjectSlug,
    currentProject,
    groups,
    projects,
    globalSkills,
    globalMcpServers,
    canViewAuditLogs,
    visibleObserveNavItems,
    extensionSettingsNavItems,
    navigate,
  ])

  const groupResultsGroup = browseResults.groups.length > 0 && (
    <>
      <CommandGroup heading={TERMS.group.plural}>
        {browseResults.groups.map((group) => (
          <CommandItem
            key={group.id}
            value={`group-${group.id}`}
            onSelect={() =>
              runWithFrecency(`group:${group.id}`, () =>
                navigate(groupHref(group.slug))
              )
            }
            className="flex items-center gap-2"
          >
            <Boxes className="h-4 w-4" />
            <span className="truncate">{group.name}</span>
            <span className="truncate font-mono text-xs text-muted-foreground">
              {group.slug}
            </span>
          </CommandItem>
        ))}
      </CommandGroup>
      <CommandSeparator />
    </>
  )

  const projectResultsGroup = browseResults.projects.length > 0 && (
    <>
      <CommandGroup heading={TERMS.service.plural}>
        {browseResults.projects.map((project) => (
          <CommandItem
            key={project.id}
            onSelect={() =>
              runWithFrecency(`project:${project.id}`, () =>
                navigate(`/projects/${project.slug}`)
              )
            }
            className="flex items-center gap-2"
          >
            <ProjectAvatar name={project.name} className="size-6" />
            <span>{project.slug}</span>
          </CommandItem>
        ))}
      </CommandGroup>
      <CommandSeparator />
    </>
  )

  const handleOpenChange = (nextOpen: boolean) => {
    setOpen(nextOpen)
    if (!nextOpen) {
      setSearch('')
      setActiveValue('')
    }
  }

  return (
    <CommandDialog
      open={open}
      onOpenChange={handleOpenChange}
      contentClassName="!bottom-auto !left-3 !right-3 !top-3 !h-auto !max-h-[calc(100dvh-1.5rem)] !w-auto !max-w-none !translate-x-0 !translate-y-0 gap-0 overflow-hidden rounded-xl p-0 shadow-lg dark:shadow-none sm:!inset-auto sm:!left-1/2 sm:!top-[18%] sm:!h-auto sm:!max-h-[min(70dvh,36rem)] sm:!w-full sm:!max-w-xl sm:!-translate-x-1/2"
    >
      <Command
        className="rounded-xl border-0 shadow-none"
        loop
        shouldFilter={false}
        value={activeValue}
        onValueChange={setActiveValue}
      >
        <CommandInput
          aria-label="Search this Temps instance"
          placeholder="Search projects, services, settings, and tools…"
          value={search}
          onValueChange={setSearch}
          className="h-12 pr-10 text-base sm:text-sm"
        />
        <CommandList className="max-h-[calc(100dvh-7rem)] min-h-0 p-2 sm:max-h-[28rem] sm:min-h-72">
          {search && <CommandEmpty>No results found.</CommandEmpty>}

          {!search && (
            <CommandPaletteSuggestions
              queries={sampleQueries}
              onSelect={setSearch}
            />
          )}

          {/* Typing: one list, best match first, regardless of section. The
              section name rides along as a right-aligned label so you can
              still tell a project page from a settings page. */}
          {search && visibleResults.length > 0 && (
            <CommandGroup heading="Results">
              {visibleResults.slice(0, 30).map((entry) => (
                <CommandItem
                  key={entry.key}
                  value={entry.key}
                  onSelect={() => runWithFrecency(entry.key, entry.run)}
                  className="flex items-center gap-2"
                >
                  {entry.icon}
                  <span className="truncate">{entry.title}</span>
                  {entry.subtitle && (
                    <span className="truncate font-mono text-xs text-muted-foreground">
                      {entry.subtitle}
                    </span>
                  )}
                  <span className="ml-auto shrink-0 pl-2 text-xs text-muted-foreground">
                    {entry.category}
                  </span>
                </CommandItem>
              ))}
            </CommandGroup>
          )}

          {search && visibleLocalMatches.length > 0 && (
            <CommandGroup heading="Instance-wide matches">
              {visibleLocalMatches.map(({ item }) => (
                <CommandItem
                  key={item.id}
                  value={`local-${item.id}`}
                  onSelect={() =>
                    runWithFrecency(item.url, () => navigate(item.url))
                  }
                  className="flex items-center gap-3 rounded-xl py-3"
                >
                  <Search className="size-4 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate font-medium">
                    {item.title}
                  </span>
                  <span className="shrink-0 text-xs text-muted-foreground">
                    {item.category}
                  </span>
                </CommandItem>
              ))}
            </CommandGroup>
          )}

          {/* Recent (frecency-ranked, only when input is empty) */}
          {showFullBrowseCatalog && !search && recentItems.length > 0 && (
            <>
              <CommandGroup heading="Recent">
                {recentItems.map((entry) => (
                  <CommandItem
                    key={`recent-${entry.key}`}
                    value={`recent-${entry.key}`}
                    onSelect={() => runWithFrecency(entry.key, entry.run)}
                    className="flex items-center gap-2"
                  >
                    {entry.icon}
                    <span className="truncate">{entry.title}</span>
                    {entry.subtitle && (
                      <span className="text-xs text-muted-foreground font-mono truncate">
                        {entry.subtitle}
                      </span>
                    )}
                  </CommandItem>
                ))}
              </CommandGroup>
              <CommandSeparator />
            </>
          )}

          {/* Project Navigation (shown first when on a project page) */}
          {!search &&
            currentProject &&
            showFullBrowseCatalog &&
            browseResults.projectNav.length > 0 && (
              <>
                <CommandGroup heading={`${currentProject?.name}`}>
                  {browseResults.projectNav.map((item) => (
                    <CommandItem
                      key={item.url}
                      value={`project-nav-${item.url}`}
                      onSelect={() =>
                        runWithFrecency(item.url, () => navigate(item.url))
                      }
                      className="flex items-center gap-2"
                    >
                      <item.icon className="h-4 w-4" />
                      <span>{item.title}</span>
                    </CommandItem>
                  ))}
                </CommandGroup>
                <CommandSeparator />
              </>
            )}

          {/* Matching projects take priority over common navigation pages. */}
          {/* Main Navigation */}
          {showFullBrowseCatalog &&
            !search &&
            browseResults.navigation.length > 0 && (
              <>
                <CommandGroup heading="Navigation">
                  {browseResults.navigation.map((item) => (
                    <CommandItem
                      key={item.url}
                      value={`nav-${item.url}`}
                      onSelect={() =>
                        runWithFrecency(item.url, () => navigate(item.url))
                      }
                      className="flex items-center gap-2"
                    >
                      <item.icon className="h-4 w-4" />
                      <span>{item.title}</span>
                    </CommandItem>
                  ))}
                </CommandGroup>
                <CommandSeparator />
              </>
            )}

          {/* Settings Navigation */}
          {showFullBrowseCatalog &&
            !search &&
            browseResults.settings.length > 0 && (
              <>
                <CommandGroup heading="Settings">
                  {browseResults.settings.map((item) => (
                    <CommandItem
                      key={item.url}
                      value={`settings-${item.url}`}
                      onSelect={() =>
                        runWithFrecency(item.url, () => navigate(item.url))
                      }
                      className="flex items-center gap-2"
                    >
                      <item.icon className="h-4 w-4" />
                      <span>{item.title}</span>
                    </CommandItem>
                  ))}
                </CommandGroup>
                <CommandSeparator />
              </>
            )}

          {/* Observe Navigation */}
          {showFullBrowseCatalog &&
            !search &&
            browseResults.observe.length > 0 && (
              <>
                <CommandGroup heading="Observe">
                  {browseResults.observe.map((item) => (
                    <CommandItem
                      key={item.url}
                      value={`observe-${item.url}`}
                      onSelect={() =>
                        runWithFrecency(item.url, () => navigate(item.url))
                      }
                      className="flex items-center gap-2"
                    >
                      <item.icon className="h-4 w-4" />
                      <span>{item.title}</span>
                    </CommandItem>
                  ))}
                </CommandGroup>
                <CommandSeparator />
              </>
            )}

          {/* Plugins Navigation */}
          {showFullBrowseCatalog &&
            !search &&
            browseResults.plugins.length > 0 && (
              <>
                <CommandGroup heading="Plugins">
                  {browseResults.plugins.map((item) => (
                    <CommandItem
                      key={item.url}
                      value={`plugins-${item.url}`}
                      onSelect={() =>
                        runWithFrecency(item.url, () => navigate(item.url))
                      }
                      className="flex items-center gap-2"
                    >
                      <item.icon className="h-4 w-4" />
                      <span>{item.title}</span>
                    </CommandItem>
                  ))}
                </CommandGroup>
                <CommandSeparator />
              </>
            )}

          {/* Account Navigation */}
          {showFullBrowseCatalog &&
            !search &&
            browseResults.account.length > 0 && (
              <>
                <CommandGroup heading="Account">
                  {browseResults.account.map((item) => (
                    <CommandItem
                      key={item.url}
                      value={`account-${item.url}`}
                      onSelect={() =>
                        runWithFrecency(item.url, () => navigate(item.url))
                      }
                      className="flex items-center gap-2"
                    >
                      <item.icon className="h-4 w-4" />
                      <span>{item.title}</span>
                    </CommandItem>
                  ))}
                </CommandGroup>
                <CommandSeparator />
              </>
            )}

          {/* Skills */}
          {showFullBrowseCatalog &&
            !search &&
            browseResults.skills.length > 0 && (
              <>
                <CommandGroup heading="Skills">
                  {browseResults.skills.slice(0, 10).map((skill) => (
                    <CommandItem
                      key={`skill-${skill.id}`}
                      onSelect={() =>
                        runWithFrecency(`skill:${skill.slug}`, () =>
                          navigate(`/skills/${skill.slug}`)
                        )
                      }
                      className="flex items-center gap-2"
                    >
                      <Wand2 className="h-4 w-4" />
                      <span className="truncate">{skill.name}</span>
                      <span className="text-xs text-muted-foreground font-mono truncate">
                        {skill.slug}
                      </span>
                    </CommandItem>
                  ))}
                </CommandGroup>
                <CommandSeparator />
              </>
            )}

          {/* MCP Servers */}
          {showFullBrowseCatalog &&
            !search &&
            browseResults.mcpServers.length > 0 && (
              <>
                <CommandGroup heading="MCP servers">
                  {browseResults.mcpServers.slice(0, 10).map((mcp) => (
                    <CommandItem
                      key={`mcp-${mcp.id}`}
                      onSelect={() =>
                        runWithFrecency(`mcp:${mcp.slug}`, () =>
                          navigate(`/mcp-servers/${mcp.slug}`)
                        )
                      }
                      className="flex items-center gap-2"
                    >
                      <Server className="h-4 w-4" />
                      <span className="truncate">{mcp.name}</span>
                      <span className="text-xs text-muted-foreground font-mono truncate">
                        {mcp.slug}
                      </span>
                    </CommandItem>
                  ))}
                </CommandGroup>
                <CommandSeparator />
              </>
            )}

          {/* Preserve the browse order when the palette opens without a query. */}
          {showFullBrowseCatalog && !search && groupResultsGroup}
          {showFullBrowseCatalog && !search && projectResultsGroup}

          {/* Actions */}
          {showFullBrowseCatalog &&
            !search &&
            browseResults.actions.length > 0 && (
              <CommandGroup heading="Actions">
                {browseResults.actions.map((action) => (
                  <CommandItem
                    key={action.id}
                    onSelect={() =>
                      runWithFrecency(`action:${action.id}`, action.run)
                    }
                    className="flex items-center gap-2"
                  >
                    <action.icon className="h-4 w-4" />
                    <span>{action.title}</span>
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
        </CommandList>
        <div className="flex items-center justify-between gap-3 border-t px-3 py-2 text-xs text-muted-foreground">
          <span className="truncate">
            {commandDestinations.length} destinations
          </span>
          <span className="flex shrink-0 items-center gap-3">
            <span>↑↓ navigate</span>
            <span>↵ open</span>
            <span className="hidden sm:inline">esc close</span>
          </span>
        </div>
      </Command>
    </CommandDialog>
  )
}
