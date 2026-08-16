import {
  getProjectsInfiniteOptions,
  listGlobalMcpsOptions,
  listGlobalSkillsOptions,
  listServicesInfiniteOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
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
import {
  buildCommandSampleQueries,
  dedupeCommandDestinations,
  resolveExplicitNamedProjectDestination,
  resolveExplicitProjectEnvironment,
  toCommandExtendedQuery,
  type CommandDestination,
} from '@/lib/command-navigation'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import Fuse from 'fuse.js'
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
  CornerDownLeft,
  Database,
  DatabaseBackup,
  FileLock2,
  Flag,
  Folder,
  FolderPlus,
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
  ScrollText,
  Search,
  Server,
  Settings,
  Settings2,
  Shield,
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
  icon: LucideIcon
  keywords?: string[]
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
    title: 'Toggle Theme',
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
    title: 'All platform tools',
    url: '/tools',
    icon: Boxes,
    keywords: ['tools', 'features', 'capabilities', 'everything'],
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
    title: 'Create New Project',
    url: '/projects/new',
    icon: FolderPlus,
    keywords: ['new', 'create', 'add', 'project', 'app'],
  },
  {
    title: 'Drop Project Files',
    url: '/projects/new?source=drop',
    icon: Upload,
    keywords: ['drop', 'upload', 'zip', 'folder', 'deploy', 'no git'],
  },
  {
    title: 'Import Project',
    url: '/projects/import-wizard',
    icon: Upload,
    keywords: ['import', 'migrate', 'workload', 'platform', 'external'],
  },
  {
    title: 'Monitoring',
    url: '/monitoring/alerts',
    icon: Gauge,
    keywords: ['metrics', 'resources', 'alerts', 'alarms', 'health'],
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
]

const settingsNavItems: NavigationItem[] = [
  // General
  {
    title: 'Platform Settings',
    url: '/settings',
    icon: Settings2,
    keywords: ['preferences', 'configuration', 'config', 'platform', 'general'],
  },
  {
    title: 'Notification Providers',
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
    title: 'Add Notification Provider',
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
    title: 'Create Team',
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
    title: 'Add SSO Provider',
    url: '/settings/auth/new',
    icon: KeyRound,
    keywords: ['sso', 'oidc', 'create', 'connect', 'okta', 'auth0', 'keycloak'],
  },
  {
    title: 'API Keys',
    url: '/settings/keys',
    icon: Key,
    keywords: ['tokens', 'auth', 'authentication', 'api'],
  },
  {
    title: 'Create API Key',
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
    title: 'Provision Domain',
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
    title: 'AI Chat',
    url: '/chat',
    icon: MessageSquare,
    keywords: ['ai', 'chat', 'assistant', 'conversation', 'ask'],
  },
  {
    title: 'AI Usage',
    url: '/ai-gateway/usage',
    icon: BarChart3,
    keywords: ['ai', 'usage', 'credits', 'cost', 'tokens', 'spend', 'billing'],
  },
  {
    title: 'AI Activity',
    url: '/ai-gateway/activity',
    icon: Activity,
    keywords: ['ai', 'activity', 'traces', 'telemetry', 'otel', 'spans'],
  },
  {
    title: 'AI Gateway Setup',
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
    title: 'AI Workflows',
    url: '/ai-workflows',
    icon: Bot,
    keywords: [
      'ai',
      'workflows',
      'agents',
      'sandbox',
      'automation',
      'autopilot',
    ],
  },
  {
    title: 'Connect AI Harness',
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
    title: 'MCP Servers',
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
    title: 'Git Providers',
    url: '/git-providers',
    icon: GitBranch,
    keywords: ['github', 'gitlab', 'version control', 'repositories'],
  },
  {
    title: 'Add Git Provider',
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
    title: 'DNS Providers',
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
    title: 'Add DNS Provider',
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
    title: 'Load Balancer',
    url: '/settings/load-balancer',
    icon: Server,
    keywords: ['lb', 'balancing', 'proxy', 'routes'],
  },
  {
    title: 'Docker Registry',
    url: '/settings/docker-registry',
    icon: Boxes,
    keywords: ['docker', 'registry', 'container', 'image'],
  },
  {
    title: 'Build Limits',
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
    title: 'Worker Nodes',
    url: '/settings/nodes',
    icon: Network,
    keywords: ['worker', 'nodes', 'cluster', 'multinode', 'infrastructure'],
  },
  {
    title: 'Plugins',
    url: '/settings/plugins',
    icon: Puzzle,
    keywords: ['plugins', 'extensions', 'addons', 'modules'],
  },
  // Security
  {
    title: 'Security Headers',
    url: '/settings/security',
    icon: Shield,
    keywords: ['security', 'headers', 'csp', 'cors', 'protection'],
  },
  {
    title: 'Rate Limiting',
    url: '/settings/rate-limiting',
    icon: Monitor,
    keywords: ['rate', 'limit', 'throttle', 'ip', 'access'],
  },
  {
    title: 'Disk Monitoring',
    url: '/settings/disk-monitoring',
    icon: HardDrive,
    keywords: ['disk', 'space', 'storage', 'alerts', 'monitoring'],
  },
  {
    title: 'Metrics Monitoring',
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
    title: 'Proxy Logs',
    url: '/proxy-logs',
    icon: Activity,
    keywords: ['logs', 'proxy', 'requests', 'traffic'],
  },
  {
    title: 'Audit Logs',
    url: '/audit-logs',
    icon: ScrollText,
    keywords: ['logs', 'audit', 'history', 'activity'],
  },
]

const accountNavItems: NavigationItem[] = [
  {
    title: 'Account',
    url: '/account',
    icon: BadgeCheck,
    keywords: ['profile', 'user', 'me'],
  },
]

// Project-specific navigation items (will be prefixed with project slug)
const projectNavItems: NavigationItem[] = [
  {
    title: 'Project Overview',
    url: 'project',
    icon: Home,
    keywords: ['home', 'overview', 'main'],
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
    title: 'Pages',
    url: 'analytics/pages',
    icon: Activity,
    keywords: ['pages', 'views', 'pageviews', 'analytics'],
  },
  {
    title: 'AI Agents',
    url: 'analytics/ai-agents',
    icon: Bot,
    keywords: ['ai', 'agents', 'bots', 'llm', 'analytics', 'traffic'],
  },
  {
    title: 'Session Replays',
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
    title: 'Analytics Setup',
    url: 'analytics/setup',
    icon: Settings,
    keywords: ['setup', 'configuration', 'install', 'analytics'],
  },
  {
    title: 'API Traffic',
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
    title: 'Log History',
    url: 'runtime?tab=history',
    icon: History,
    keywords: ['logs', 'history', 'search', 'archive', 'past'],
  },
  {
    title: 'Speed Insights',
    url: 'speed',
    icon: Monitor,
    keywords: ['performance', 'speed', 'insights', 'vitals'],
  },
  {
    title: 'Error Tracking',
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
    title: 'AI Crawlers',
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
    title: 'Project Settings',
    url: 'settings/general',
    icon: Settings,
    keywords: ['settings', 'configuration', 'general'],
  },
  {
    title: 'Feature Flags',
    url: 'flags',
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
    title: 'Environment Variables',
    url: 'environment-variables',
    icon: Key,
    keywords: ['variables', 'env', 'config'],
  },
  {
    title: 'Secrets',
    url: 'settings/secrets',
    icon: FileLock2,
    keywords: ['secrets', 'secret files', 'mounted secrets', '/run/secrets'],
  },
  {
    title: 'Git',
    url: 'git',
    icon: GitBranch,
    keywords: ['git', 'repository', 'repo', 'source'],
  },
  {
    title: 'Build & Deploy',
    url: 'build',
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
    title: 'Access',
    url: 'settings/access',
    icon: Users,
    keywords: ['access', 'permissions', 'members', 'roles', 'team'],
  },
  {
    title: 'Cron Jobs',
    url: 'settings/cron-jobs',
    icon: Activity,
    keywords: ['cron', 'jobs', 'scheduled', 'tasks'],
  },
  {
    title: 'Webhooks',
    url: 'settings/webhooks',
    icon: Workflow,
    keywords: ['webhooks', 'hooks', 'events', 'callbacks', 'integrations'],
  },
  {
    title: 'Project Skills',
    url: 'settings/skills',
    icon: Wand2,
    keywords: ['skills', 'ai', 'agents', 'claude', 'instructions', 'project'],
  },
  {
    title: 'Project MCP Servers',
    url: 'settings/mcp-servers',
    icon: Server,
    keywords: [
      'mcp',
      'model',
      'context',
      'protocol',
      'tools',
      'servers',
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
    title: 'Observe',
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
    title: 'Telemetry Logs',
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
    title: 'Services - KV Store',
    url: 'services/kv',
    icon: Database,
    keywords: ['kv', 'key-value', 'redis', 'cache', 'storage'],
  },
  {
    title: 'Services - Blob Storage',
    url: 'services/blob',
    icon: HardDrive,
    keywords: ['blob', 's3', 'files', 'storage', 'uploads', 'objects'],
  },
  {
    title: 'AI Traces',
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
    title: 'Agents',
    url: 'agents',
    icon: Bot,
    keywords: ['agents', 'autopilot', 'ai', 'automation', 'workflows'],
  },
  {
    title: 'Autofixer',
    url: 'autofixer',
    icon: Wand2,
    keywords: ['autofix', 'autofixer', 'ai', 'errors', 'repair'],
  },
  {
    title: 'Workspace',
    url: 'workspace',
    icon: SquareTerminal,
    keywords: ['workspace', 'shell', 'terminal', 'exec'],
  },
  {
    title: 'Error Alert Rules',
    url: 'errors/alert-rules',
    icon: Bell,
    keywords: ['errors', 'alerts', 'rules', 'notifications'],
  },
  {
    title: 'Security Scans',
    url: 'security',
    icon: Shield,
    keywords: ['security', 'scans', 'vulnerabilities', 'cve'],
  },
  {
    title: 'Request Logs',
    url: 'request-logs',
    icon: Network,
    keywords: ['logs', 'requests', 'http', 'traffic'],
  },
]

export function CommandPalette() {
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState('')
  const navigate = useNavigate()
  const location = useLocation()
  const { plugins, projectNavEntries } = usePluginsContext()
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
      refetchSkills()
      refetchMcp()
      refetchServices()
    }
  }, [open, refetchProjects, refetchSkills, refetchMcp, refetchServices])

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

  // Build plugin navigation items for the command palette
  const pluginNavItems: NavigationItem[] = useMemo(
    () =>
      plugins.flatMap((p) =>
        p.nav
          .filter((e) => e.section !== 'project')
          .map((entry) => ({
            title: entry.label,
            url: entry.path,
            icon: resolvePluginIcon(entry.icon),
            keywords: ['plugin', p.name, entry.label.toLowerCase()],
          }))
      ),
    [plugins]
  )

  // Project-scoped plugin nav entries (relative URLs, prefixed at render time)
  const projectPluginNavItems: NavigationItem[] = useMemo(
    () =>
      projectNavEntries.map((entry) => ({
        title: entry.label,
        url: entry.path,
        icon: resolvePluginIcon(entry.icon),
        keywords: ['plugin', 'project', entry.label.toLowerCase()],
      })),
    [projectNavEntries]
  )

  const canViewAuditLogs = useCanViewAuditLogs()

  // Create Fuse instances for fuzzy search
  const navFuse = useMemo(() => {
    const allNavItems = [
      ...mainNavItems.map((item) => ({ ...item, category: 'Navigation' })),
      ...settingsNavItems.map((item) => ({ ...item, category: 'Settings' })),
      ...observeNavItems
        .filter((item) => canViewAuditLogs || item.url !== '/audit-logs')
        .map((item) => ({ ...item, category: 'Observe' })),
      ...accountNavItems.map((item) => ({ ...item, category: 'Account' })),
      ...pluginNavItems.map((item) => ({ ...item, category: 'Plugins' })),
    ]

    // Add project-specific navigation if we're on a project page
    if (currentProjectSlug && currentProject) {
      const projectSpecificItems = [
        ...projectNavItems,
        ...projectPluginNavItems,
      ].map((item) => ({
        ...item,
        // Prepend project slug to URL for absolute navigation
        url: `/projects/${currentProjectSlug}/${item.url}`,
        category: 'Project',
      }))
      allNavItems.push(...projectSpecificItems)
    }

    return new Fuse(allNavItems, {
      keys: [
        { name: 'title', weight: 2 },
        { name: 'url', weight: 1 },
        { name: 'keywords', weight: 1.5 },
      ],
      threshold: 0.3,
      includeScore: true,
      shouldSort: true,
      minMatchCharLength: 1,
    })
  }, [
    currentProjectSlug,
    currentProject,
    pluginNavItems,
    projectPluginNavItems,
    canViewAuditLogs,
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
      pages.map((page) => ({
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
        ? [...projectNavItems, ...projectPluginNavItems].map((item) => ({
            ...item,
            url: `/projects/${currentProjectSlug}/${item.url}`,
          }))
        : []

    return {
      navigation: mainNavItems,
      settings: settingsNavItems,
      observe: observeNavItems.filter(
        (item) => canViewAuditLogs || item.url !== '/audit-logs'
      ),
      account: accountNavItems,
      plugins: pluginNavItems,
      projectNav: projectNavigation,
      projects,
      skills: globalSkills,
      mcpServers: globalMcpServers,
      actions: commandActions,
    }
  }, [
    projects,
    globalSkills,
    globalMcpServers,
    pluginNavItems,
    projectPluginNavItems,
    currentProjectSlug,
    currentProject,
    canViewAuditLogs,
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
          ? `Current project · ${project.slug}`
          : `Project · ${project.slug}`
      const pages = projectNavItems.map((item) => {
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
              project.id === currentProject?.id ? 'current project' : '',
            ].filter(Boolean),
          },
          category,
          `Open ${item.title} for project ${project.name} (${project.slug})`
        )
      })

      pages.push({
        id: `project:${project.slug}:environment:production`,
        title: `Production environment · ${project.slug}`,
        description: `Open the production environment for project ${project.name} (${project.slug})`,
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

    const servicePages: CommandDestination[] = services.map((service) => ({
      id: `service:${service.id}`,
      title: `${service.name} · ${service.service_type}`,
      description: `Open the ${service.service_type} service named ${service.name}`,
      url: `/storage/${service.id}`,
      category: 'Service',
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
      ...projectPages,
      ...servicePages,
      ...skillPages,
      ...mcpPages,
    ])
  }, [
    browseResults,
    currentProject,
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
          item.category === 'Project' && currentProject
            ? currentProject.name
            : undefined,
        category: item.category,
        icon: <Icon className="h-4 w-4" />,
        score: rank(item.url, item.title, result.score),
        run: () => navigate(item.url),
      })
    }

    for (const result of projectsFuse.search(search)) {
      const project = result.item
      out.push({
        key: `project:${project.id}`,
        title: project.slug,
        category: 'Project',
        icon: (
          <Avatar className="size-5">
            <AvatarImage src={`/api/projects/${project.id}/favicon`} />
            <AvatarFallback>{project.name.charAt(0)}</AvatarFallback>
          </Avatar>
        ),
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
          category: 'Project',
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

  const getConstrainedLocalMatch = () => {
    const query = search.trim()
    if (!query) return undefined

    const explicitProjectEnvironment = resolveExplicitProjectEnvironment(
      query,
      projects.map((project) => project.slug),
      commandDestinations,
      currentProjectSlug
    )
    if (explicitProjectEnvironment) return explicitProjectEnvironment

    return resolveExplicitNamedProjectDestination(
      query,
      projects.map((project) => project.slug),
      localMatches.map(({ item }) => item)
    )
  }

  const openBestLocalMatch = () => {
    if (!search.trim()) return false

    const constrainedDestination = getConstrainedLocalMatch()
    if (constrainedDestination) {
      runWithFrecency(constrainedDestination.url, () =>
        navigate(constrainedDestination.url)
      )
      return true
    }

    const rankedResult = rankedResults[0]
    if (rankedResult) {
      runWithFrecency(rankedResult.key, rankedResult.run)
      return true
    }

    const destination = localMatches[0]?.item
    if (!destination) return false
    runWithFrecency(destination.url, () => navigate(destination.url))
    return true
  }

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
      ...mainNavItems,
      ...settingsNavItems,
      ...observeNavItems,
      ...accountNavItems,
      ...pluginNavItems,
    ]
    const projectNavigation =
      currentProjectSlug && currentProject
        ? [...projectNavItems, ...projectPluginNavItems].map((item) => ({
            ...item,
            url: `/projects/${currentProjectSlug}/${item.url}`,
          }))
        : []
    const navByUrl = new Map<string, NavigationItem>()
    for (const nav of [...allNavItems, ...projectNavigation]) {
      navByUrl.set(nav.url, nav)
    }
    const projectsById = new Map(projects.map((p) => [String(p.id), p]))
    const skillsBySlug = new Map(globalSkills.map((s) => [s.slug, s]))
    const mcpBySlug = new Map(globalMcpServers.map((m) => [m.slug, m]))

    const out: RecentEntry[] = []
    for (const key of recent(7)) {
      if (key.startsWith('project:')) {
        const project = projectsById.get(key.slice('project:'.length))
        if (!project) continue
        out.push({
          key,
          title: project.slug,
          icon: (
            <Avatar className="size-5">
              <AvatarImage src={`/api/projects/${project.id}/favicon`} />
              <AvatarFallback>{project.name.charAt(0)}</AvatarFallback>
            </Avatar>
          ),
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
    projects,
    globalSkills,
    globalMcpServers,
    navigate,
  ])

  const projectResultsGroup = browseResults.projects.length > 0 && (
    <>
      <CommandGroup heading="Projects">
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
            <Avatar className="size-6">
              <AvatarImage src={`/api/projects/${project.id}/favicon`} />
              <AvatarFallback>{project.name.charAt(0)}</AvatarFallback>
            </Avatar>
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
    }
  }

  return (
    <CommandDialog
      open={open}
      onOpenChange={handleOpenChange}
      contentClassName="!inset-0 !left-0 !top-0 !h-dvh !max-h-dvh !w-screen !max-w-none !translate-x-0 !translate-y-0 gap-0 overflow-hidden !rounded-none !border-0 p-0 shadow-none sm:!inset-auto sm:!left-1/2 sm:!top-[12%] sm:!h-auto sm:!max-h-[calc(100dvh-1rem)] sm:!w-full sm:!max-w-3xl sm:!-translate-x-1/2 sm:!rounded-3xl sm:!border sm:border-white/40 sm:shadow-[0_32px_100px_rgba(16,20,30,0.28)]"
    >
      <Command
        className="rounded-none border-0 shadow-none"
        loop
        shouldFilter={false}
      >
        <div className="relative overflow-hidden border-b bg-muted/40 px-5 pb-4 pt-5 sm:px-6">
          <div className="pointer-events-none absolute -right-16 -top-24 size-64 rounded-full bg-blue-500/10 blur-3xl" />
          <div className="relative mb-4 flex items-center gap-3 pr-10">
            <span className="grid size-9 shrink-0 place-items-center rounded-xl bg-foreground text-background">
              <Search className="size-5" />
            </span>
            <div className="min-w-0">
              <p className="truncate text-base font-semibold sm:text-lg">
                Search this Temps instance
              </p>
              <p className="hidden text-xs text-muted-foreground sm:block">
                Find projects, environments, services, settings, and tools
              </p>
            </div>
            <span
              className="ml-auto shrink-0 rounded-full border border-emerald-500/20 bg-emerald-500/10 px-3 py-1 font-mono text-[10px] font-semibold uppercase tracking-wider text-emerald-700 dark:text-emerald-300"
              title="Instant local navigation is ready"
            >
              search · ready
            </span>
          </div>
          <CommandInput
            placeholder={'Try “production environment for project-slug”'}
            value={search}
            onValueChange={setSearch}
            onKeyDown={(event) => {
              if (event.key !== 'Enter') return
              event.preventDefault()
              event.stopPropagation()
              openBestLocalMatch()
            }}
            className="h-14 text-base font-medium"
          />
          <div className="mt-2 flex justify-end gap-2 pr-1 text-[10px] text-muted-foreground">
            <kbd className="rounded-md border bg-background/70 px-2 py-1 font-mono">
              <CornerDownLeft className="mr-1 inline size-3" /> open
            </kbd>
          </div>
        </div>
        <CommandList className="max-h-none min-h-0 flex-1 p-3 sm:max-h-[56vh] sm:min-h-80 sm:flex-none">
          {search && <CommandEmpty>No results found.</CommandEmpty>}

          {!search && (
            <div className="p-2">
              <p className="mb-3 font-mono text-[10px] font-semibold uppercase tracking-[0.18em] text-muted-foreground">
                Ask in your own words
              </p>
              <div className="grid gap-2 sm:grid-cols-2">
                {sampleQueries.map((query) => (
                  <button
                    key={query}
                    type="button"
                    onClick={() => setSearch(query)}
                    className="group flex min-h-20 items-start gap-3 rounded-xl border bg-muted/25 p-4 text-left transition-colors hover:border-ring/30 hover:bg-accent"
                  >
                    <Search className="mt-0.5 size-4 shrink-0 text-muted-foreground group-hover:text-foreground" />
                    <span className="text-sm font-medium leading-relaxed">
                      {query}
                    </span>
                  </button>
                ))}
              </div>
              <div className="mt-4 flex items-center gap-3 rounded-xl bg-muted/50 px-4 py-3 text-xs text-muted-foreground">
                <span className="size-2 shrink-0 rounded-full bg-emerald-500" />
                Search is instant and matched locally against this instance.
              </div>
            </div>
          )}

          {/* Typing: one list, best match first, regardless of section. The
              section name rides along as a right-aligned label so you can
              still tell a project page from a settings page. */}
          {search && rankedResults.length > 0 && (
            <CommandGroup heading="Results">
              {rankedResults.slice(0, 30).map((entry) => (
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

          {search && localMatches.length > 0 && (
            <CommandGroup heading="Instance-wide matches">
              {localMatches
                .filter(
                  ({ item }) =>
                    !rankedResults.some((result) => result.key === item.url)
                )
                .slice(0, 8)
                .map(({ item }) => (
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
                <CommandGroup heading="MCP Servers">
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
        <div className="flex items-center gap-2 border-t bg-muted/30 px-5 py-3 text-[10px] text-muted-foreground sm:text-xs">
          <span className="size-1.5 rounded-full bg-emerald-500" />
          <span>Local search</span>
          <span className="hidden sm:inline">
            {commandDestinations.length} destinations indexed
          </span>
        </div>
      </Command>
    </CommandDialog>
  )
}
