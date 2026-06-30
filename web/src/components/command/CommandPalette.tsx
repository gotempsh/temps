import {
  getProjectsOptions,
  listGlobalMcpsOptions,
  listGlobalSkillsOptions,
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
import { useFrecency } from '@/hooks/useFrecency'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { useQuery } from '@tanstack/react-query'
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
  Database,
  DatabaseBackup,
  FileLock2,
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
  Monitor,
  Network,
  Puzzle,
  ScrollText,
  Server,
  Settings,
  Settings2,
  Shield,
  Sparkles,
  SquareTerminal,
  Upload,
  Users,
  Wand2,
  Workflow,
  type LucideIcon,
} from 'lucide-react'
import { useEffect, useMemo, useState, type ReactNode } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'

interface NavigationItem {
  title: string
  url: string
  icon: LucideIcon
  keywords?: string[]
}

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
    title: 'Sandboxes',
    url: '/sandboxes',
    icon: Box,
    keywords: ['sandbox', 'sandboxes', 'workspace', 'shell', 'terminal', 'environment'],
  },
  {
    title: 'Create New Project',
    url: '/projects/new',
    icon: FolderPlus,
    keywords: ['new', 'create', 'add', 'project', 'app'],
  },
  {
    title: 'Import Project',
    url: '/projects/import-wizard',
    icon: Upload,
    keywords: ['import', 'migrate', 'workload', 'platform', 'external'],
  },
  {
    title: 'Monitoring',
    url: '/monitoring',
    icon: Activity,
    keywords: ['metrics', 'performance', 'analytics', 'stats', 'alerts', 'health'],
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
    keywords: ['alerts', 'notifications', 'providers', 'slack', 'email', 'webhook'],
  },
  {
    title: 'Add Notification Provider',
    url: '/monitoring/providers/add',
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
  // Infrastructure
  {
    title: 'Domains',
    url: '/domains',
    icon: Globe,
    keywords: ['dns', 'urls', 'websites', 'custom domain'],
  },
  {
    title: 'Databases',
    url: '/storage',
    icon: Database,
    keywords: ['database', 'databases', 'storage', 'files', 'data', 'services'],
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
    keywords: ['ai', 'llm', 'openai', 'anthropic', 'gateway', 'models', 'providers', 'chat', 'gpt', 'claude'],
  },
  {
    title: 'AI Workflows',
    url: '/agent-sandbox',
    icon: Bot,
    keywords: ['ai', 'workflows', 'agents', 'sandbox', 'automation', 'autopilot'],
  },
  {
    title: 'Skills',
    url: '/skills',
    icon: Wand2,
    keywords: ['skills', 'ai', 'agents', 'claude', 'instructions', 'prompts', 'global'],
  },
  {
    title: 'MCP Servers',
    url: '/mcp-servers',
    icon: Server,
    keywords: ['mcp', 'model', 'context', 'protocol', 'tools', 'servers', 'agents', 'claude', 'global'],
  },
  {
    title: 'Git Providers',
    url: '/git-providers',
    icon: GitBranch,
    keywords: ['github', 'gitlab', 'version control', 'repositories'],
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
    keywords: ['restore', 'backup', 'recovery', 's3'],
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
    keywords: ['metrics', 'monitoring', 'thresholds', 'alerts', 'cpu', 'memory', 'resources'],
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
    keywords: ['traces', 'opentelemetry', 'otel', 'spans', 'tracing', 'distributed'],
  },
  {
    title: 'AI Crawlers',
    url: 'ai-crawlers',
    icon: Bot,
    keywords: ['ai', 'crawlers', 'bots', 'gptbot', 'googlebot', 'scrapers', 'observe'],
  },
  {
    title: 'Project Settings',
    url: 'settings/general',
    icon: Settings,
    keywords: ['settings', 'configuration', 'general'],
  },
  {
    title: 'Project Domains',
    url: 'settings/domains',
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
    url: 'settings/environment-variables',
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
    title: 'Git Settings',
    url: 'settings/git',
    icon: GitBranch,
    keywords: ['git', 'repository', 'repo', 'source'],
  },
  {
    title: 'Security',
    url: 'settings/security',
    icon: Shield,
    keywords: ['security', 'headers', 'rate limiting', 'protection'],
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
    keywords: ['mcp', 'model', 'context', 'protocol', 'tools', 'servers', 'project'],
  },
  {
    title: 'Metrics',
    url: 'metrics',
    icon: BarChart3,
    keywords: ['metrics', 'opentelemetry', 'otel', 'cpu', 'memory', 'resources', 'observe'],
  },
  {
    title: 'Observe',
    url: 'observe',
    icon: Activity,
    keywords: ['observe', 'events', 'opentelemetry', 'otel', 'timeline', 'all events'],
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
    keywords: ['ai', 'traces', 'observability', 'llm', 'openai', 'anthropic', 'models', 'gateway', 'otel', 'gen_ai'],
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

  const { data: projectResponse, refetch: refetchProjects } = useQuery({
    ...getProjectsOptions({
      query: {
        page: 1,
        per_page: 100,
      },
    }),
  })
  const projects = useMemo(
    () => projectResponse?.projects || [],
    [projectResponse]
  )

  const { data: globalSkillsData, refetch: refetchSkills } = useQuery({
    ...listGlobalSkillsOptions(),
    enabled: open,
    staleTime: 60_000,
  })
  const globalSkills = globalSkillsData?.items ?? []

  const { data: globalMcpServersData, refetch: refetchMcp } = useQuery({
    ...listGlobalMcpsOptions(),
    enabled: open,
    staleTime: 60_000,
  })
  const globalMcpServers = globalMcpServersData?.items ?? []

  // Detect if user is on a project page and extract slug
  const currentProjectSlug = useMemo(() => {
    const match = location.pathname.match(/^\/projects\/([^/]+)/)
    return match ? match[1] : null
  }, [location.pathname])

  const currentProject = useMemo(() => {
    if (!currentProjectSlug) return null
    return projects.find((p) => p.slug === currentProjectSlug)
  }, [currentProjectSlug, projects])
  // Refetch when the dialog is opened or when react-query invalidates
  useEffect(() => {
    if (open) {
      refetchProjects()
      refetchSkills()
      refetchMcp()
    }
  }, [open, refetchProjects, refetchSkills, refetchMcp])

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

  const { record, blend, recent } = useFrecency()

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

  // Create Fuse instances for fuzzy search
  const navFuse = useMemo(() => {
    const allNavItems = [
      ...mainNavItems.map((item) => ({ ...item, category: 'Navigation' })),
      ...settingsNavItems.map((item) => ({ ...item, category: 'Settings' })),
      ...observeNavItems.map((item) => ({ ...item, category: 'Observe' })),
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
  }, [currentProjectSlug, currentProject, pluginNavItems, projectPluginNavItems])

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

  // Perform fuzzy search
  const searchResults = useMemo(() => {
    // Prepare project navigation with full URLs
    const projectNavigation =
      currentProjectSlug && currentProject
        ? [...projectNavItems, ...projectPluginNavItems].map((item) => ({
            ...item,
            url: `/projects/${currentProjectSlug}/${item.url}`,
          }))
        : []

    if (!search) {
      return {
        navigation: mainNavItems,
        settings: settingsNavItems,
        observe: observeNavItems,
        account: accountNavItems,
        plugins: pluginNavItems,
        projectNav: projectNavigation,
        projects: projects,
        skills: globalSkills,
        mcpServers: globalMcpServers,
        actions: ['toggle-theme'],
      }
    }

    // Search navigation items
    const navResults = navFuse.search(search)
    const groupedNavResults = {
      navigation: [] as Array<{ item: NavigationItem; score: number }>,
      settings: [] as Array<{ item: NavigationItem; score: number }>,
      observe: [] as Array<{ item: NavigationItem; score: number }>,
      account: [] as Array<{ item: NavigationItem; score: number }>,
      plugins: [] as Array<{ item: NavigationItem; score: number }>,
      projectNav: [] as Array<{ item: NavigationItem; score: number }>,
    }

    navResults.forEach((result) => {
      const item = result.item
      const baseItem: NavigationItem = {
        title: item.title,
        url: item.url,
        icon: item.icon,
        keywords: item.keywords,
      }
      // Fuse score: 0 = perfect match, 1 = no match. Invert to relevance.
      const relevance = 1 - (result.score ?? 0)
      const ranked = { item: baseItem, score: blend(item.url, relevance) }

      if (item.category === 'Navigation') {
        groupedNavResults.navigation.push(ranked)
      } else if (item.category === 'Settings') {
        groupedNavResults.settings.push(ranked)
      } else if (item.category === 'Observe') {
        groupedNavResults.observe.push(ranked)
      } else if (item.category === 'Account') {
        groupedNavResults.account.push(ranked)
      } else if (item.category === 'Plugins') {
        groupedNavResults.plugins.push(ranked)
      } else if (item.category === 'Project') {
        groupedNavResults.projectNav.push(ranked)
      }
    })

    const sortByScore = (
      list: Array<{ item: NavigationItem; score: number }>
    ): NavigationItem[] =>
      list.sort((a, b) => b.score - a.score).map((entry) => entry.item)

    // Search projects, blended with frecency
    const projectResults = projectsFuse.search(search)
    const filteredProjects = projectResults
      .map((result) => ({
        item: result.item,
        score: blend(`project:${result.item.id}`, 1 - (result.score ?? 0)),
      }))
      .sort((a, b) => b.score - a.score)
      .map((entry) => entry.item)

    // Search skills & mcp servers, blended with frecency
    const filteredSkills = skillsFuse
      .search(search)
      .map((r) => ({
        item: r.item,
        score: blend(`skill:${r.item.slug}`, 1 - (r.score ?? 0)),
      }))
      .sort((a, b) => b.score - a.score)
      .map((entry) => entry.item)
    const filteredMcp = mcpFuse
      .search(search)
      .map((r) => ({
        item: r.item,
        score: blend(`mcp:${r.item.slug}`, 1 - (r.score ?? 0)),
      }))
      .sort((a, b) => b.score - a.score)
      .map((entry) => entry.item)

    // Search actions (simple fuzzy match for now)
    const actions: string[] = []
    const themeKeywords = ['toggle', 'theme', 'dark', 'light', 'mode']
    const themeFuse = new Fuse(themeKeywords, { threshold: 0.4 })
    if (themeFuse.search(search).length > 0) {
      actions.push('toggle-theme')
    }

    return {
      navigation: sortByScore(groupedNavResults.navigation),
      settings: sortByScore(groupedNavResults.settings),
      observe: sortByScore(groupedNavResults.observe),
      account: sortByScore(groupedNavResults.account),
      plugins: sortByScore(groupedNavResults.plugins),
      projectNav: sortByScore(groupedNavResults.projectNav),
      projects: filteredProjects,
      skills: filteredSkills,
      mcpServers: filteredMcp,
      actions: actions,
    }
  }, [
    search,
    navFuse,
    projectsFuse,
    skillsFuse,
    mcpFuse,
    projects,
    globalSkills,
    globalMcpServers,
    pluginNavItems,
    projectPluginNavItems,
    currentProjectSlug,
    currentProject,
    blend,
  ])

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
      } else if (key === 'action:toggle-theme') {
        out.push({
          key,
          title: 'Toggle Theme',
          icon: <Settings className="h-4 w-4" />,
          run: () => document.body.classList.toggle('dark'),
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

  return (
    <CommandDialog
      open={open}
      onOpenChange={setOpen}
      contentClassName="sm:max-w-2xl"
    >
      <Command className="rounded-lg border shadow-md" loop shouldFilter={false}>
        <CommandInput
          placeholder="Type a command or search..."
          value={search}
          onValueChange={setSearch}
        />
        <CommandList className="max-h-[60vh]">
          <CommandEmpty>No results found.</CommandEmpty>

          {/* Recent (frecency-ranked, only when input is empty) */}
          {!search && recentItems.length > 0 && (
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
          {searchResults.projectNav.length > 0 && currentProject && (
            <>
              <CommandGroup heading={`${currentProject.name}`}>
                {searchResults.projectNav.map((item) => (
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

          {/* Main Navigation */}
          {searchResults.navigation.length > 0 && (
            <>
              <CommandGroup heading="Navigation">
                {searchResults.navigation.map((item) => (
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
          {searchResults.settings.length > 0 && (
            <>
              <CommandGroup heading="Settings">
                {searchResults.settings.map((item) => (
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
          {searchResults.observe.length > 0 && (
            <>
              <CommandGroup heading="Observe">
                {searchResults.observe.map((item) => (
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
          {searchResults.plugins.length > 0 && (
            <>
              <CommandGroup heading="Plugins">
                {searchResults.plugins.map((item) => (
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
          {searchResults.account.length > 0 && (
            <>
              <CommandGroup heading="Account">
                {searchResults.account.map((item) => (
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
          {searchResults.skills.length > 0 && (
            <>
              <CommandGroup heading="Skills">
                {searchResults.skills.slice(0, 10).map((skill) => (
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
          {searchResults.mcpServers.length > 0 && (
            <>
              <CommandGroup heading="MCP Servers">
                {searchResults.mcpServers.slice(0, 10).map((mcp) => (
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

          {/* Projects */}
          {searchResults.projects.length > 0 && (
            <>
              <CommandGroup heading="Projects">
                {searchResults.projects.map((project) => (
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
                      <AvatarImage
                        src={`/api/projects/${project.id}/favicon`}
                      />
                      <AvatarFallback>{project.name.charAt(0)}</AvatarFallback>
                    </Avatar>
                    <span>{project.slug}</span>
                  </CommandItem>
                ))}
              </CommandGroup>
              <CommandSeparator />
            </>
          )}

          {/* Actions */}
          {searchResults.actions.includes('toggle-theme') && (
            <CommandGroup heading="Actions">
              <CommandItem
                onSelect={() =>
                  runWithFrecency('action:toggle-theme', () =>
                    document.body.classList.toggle('dark')
                  )
                }
              >
                <span>Toggle Theme</span>
              </CommandItem>
            </CommandGroup>
          )}
        </CommandList>
      </Command>
    </CommandDialog>
  )
}
