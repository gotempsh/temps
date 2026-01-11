import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
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
import { useQuery } from '@tanstack/react-query'
import Fuse from 'fuse.js'
import {
  Activity,
  BadgeCheck,
  BarChart3,
  Bell,
  BellPlus,
  Boxes,
  Database,
  DatabaseBackup,
  Folder,
  FolderPlus,
  GitBranch,
  Globe,
  GlobeLock,
  HardDrive,
  Home,
  Key,
  Mail,
  Monitor,
  Network,
  ScrollText,
  Server,
  Settings,
  Shield,
  SquareTerminal,
  Upload,
  Users,
  type LucideIcon,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
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
    title: 'Storage',
    url: '/storage',
    icon: Database,
    keywords: ['database', 'files', 'data'],
  },
  {
    title: 'Domains',
    url: '/domains',
    icon: Globe,
    keywords: ['dns', 'urls', 'websites'],
  },
  {
    title: 'Monitoring',
    url: '/monitoring',
    icon: Activity,
    keywords: ['metrics', 'performance', 'analytics', 'stats'],
  },
  {
    title: 'Emails',
    url: '/email',
    icon: Mail,
    keywords: ['email', 'mail', 'smtp', 'transactional', 'send'],
  },
]

const settingsNavItems: NavigationItem[] = [
  {
    title: 'Settings',
    url: '/settings',
    icon: Settings,
    keywords: ['preferences', 'configuration', 'config'],
  },
  {
    title: 'External Connectivity',
    url: '/setup/connectivity',
    icon: Network,
    keywords: ['connections', 'integrations', 'external'],
  },
  {
    title: 'API Keys',
    url: '/keys',
    icon: Key,
    keywords: ['tokens', 'auth', 'authentication', 'api'],
  },
  {
    title: 'Users',
    url: '/users',
    icon: Users,
    keywords: ['team', 'members', 'people', 'accounts'],
  },
  {
    title: 'Load Balancer',
    url: '/load-balancer',
    icon: Server,
    keywords: ['lb', 'balancing', 'proxy'],
  },
  {
    title: 'Git Providers',
    url: '/git-sources',
    icon: GitBranch,
    keywords: ['github', 'gitlab', 'version control', 'repositories'],
  },
  {
    title: 'DNS Providers',
    url: '/dns-providers',
    icon: GlobeLock,
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
    icon: GlobeLock,
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
    title: 'Notifications',
    url: '/notifications',
    icon: Bell,
    keywords: ['alerts', 'notifications', 'messages'],
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
  {
    title: 'Backups',
    url: '/backups',
    icon: DatabaseBackup,
    keywords: ['restore', 'backup', 'recovery'],
  },
  {
    title: 'Proxy Logs',
    url: '/proxy-logs',
    icon: Activity,
    keywords: ['logs', 'proxy', 'requests', 'traffic'],
  },
  {
    title: 'Audit Logs',
    url: '/settings/audit-logs',
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
    title: 'Analytics Overview',
    url: 'analytics',
    icon: BarChart3,
    keywords: ['stats', 'metrics', 'analytics'],
  },
  {
    title: 'Analytics - Visitors',
    url: 'analytics/visitors',
    icon: Users,
    keywords: ['users', 'visitors', 'traffic'],
  },
  {
    title: 'Analytics - Pages',
    url: 'analytics/pages',
    icon: Activity,
    keywords: ['pages', 'views', 'pageviews'],
  },
  {
    title: 'Analytics - Replays',
    url: 'analytics/replays',
    icon: Monitor,
    keywords: ['session', 'replays', 'recordings'],
  },
  {
    title: 'Analytics - Funnels',
    url: 'analytics/funnels',
    icon: BarChart3,
    keywords: ['funnels', 'conversion', 'flow'],
  },
  {
    title: 'Analytics - Logs',
    url: 'analytics/requests',
    icon: ScrollText,
    keywords: ['logs', 'requests', 'http'],
  },
  {
    title: 'Analytics - Setup',
    url: 'analytics/setup',
    icon: Settings,
    keywords: ['setup', 'configuration', 'install'],
  },
  {
    title: 'Storage',
    url: 'storage',
    icon: Database,
    keywords: ['database', 'storage', 'data'],
  },
  {
    title: 'Runtime Logs',
    url: 'runtime',
    icon: ScrollText,
    keywords: ['logs', 'runtime', 'console', 'output'],
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
    title: 'Monitors',
    url: 'monitors',
    icon: Activity,
    keywords: ['monitoring', 'uptime', 'health'],
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
    keywords: ['variables', 'env', 'secrets', 'config'],
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
]

export function CommandPalette() {
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState('')
  const navigate = useNavigate()
  const location = useLocation()

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

  // Detect if user is on a project page and extract slug
  const currentProjectSlug = useMemo(() => {
    const match = location.pathname.match(/^\/projects\/([^/]+)/)
    return match ? match[1] : null
  }, [location.pathname])

  const currentProject = useMemo(() => {
    if (!currentProjectSlug) return null
    return projects.find((p) => p.slug === currentProjectSlug)
  }, [currentProjectSlug, projects])
  // Refetch projects when the dialog is opened or when react-query invalidates projects
  useEffect(() => {
    if (open) {
      refetchProjects()
    }
  }, [open, refetchProjects])

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

  const runCommand = (command: () => void) => {
    setOpen(false)
    setSearch('')
    command()
  }

  // Create Fuse instances for fuzzy search
  const navFuse = useMemo(() => {
    const allNavItems = [
      ...mainNavItems.map((item) => ({ ...item, category: 'Navigation' })),
      ...settingsNavItems.map((item) => ({ ...item, category: 'Settings' })),
      ...accountNavItems.map((item) => ({ ...item, category: 'Account' })),
    ]

    // Add project-specific navigation if we're on a project page
    if (currentProjectSlug && currentProject) {
      const projectSpecificItems = projectNavItems.map((item) => ({
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
  }, [currentProjectSlug, currentProject])

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

  // Perform fuzzy search
  const searchResults = useMemo(() => {
    // Prepare project navigation with full URLs
    const projectNavigation =
      currentProjectSlug && currentProject
        ? projectNavItems.map((item) => ({
            ...item,
            url: `/projects/${currentProjectSlug}/${item.url}`,
          }))
        : []

    if (!search) {
      return {
        navigation: mainNavItems,
        settings: settingsNavItems,
        account: accountNavItems,
        projectNav: projectNavigation,
        projects: projects,
        actions: ['toggle-theme'],
      }
    }

    // Search navigation items
    const navResults = navFuse.search(search)
    const groupedNavResults = {
      navigation: [] as NavigationItem[],
      settings: [] as NavigationItem[],
      account: [] as NavigationItem[],
      projectNav: [] as NavigationItem[],
    }

    navResults.forEach((result) => {
      const item = result.item
      const baseItem: NavigationItem = {
        title: item.title,
        url: item.url,
        icon: item.icon,
        keywords: item.keywords,
      }

      if (item.category === 'Navigation') {
        groupedNavResults.navigation.push(baseItem)
      } else if (item.category === 'Settings') {
        groupedNavResults.settings.push(baseItem)
      } else if (item.category === 'Account') {
        groupedNavResults.account.push(baseItem)
      } else if (item.category === 'Project') {
        groupedNavResults.projectNav.push(baseItem)
      }
    })

    // Search projects
    const projectResults = projectsFuse.search(search)
    const filteredProjects = projectResults.map((result) => result.item)

    // Search actions (simple fuzzy match for now)
    const actions: string[] = []
    const themeKeywords = ['toggle', 'theme', 'dark', 'light', 'mode']
    const themeFuse = new Fuse(themeKeywords, { threshold: 0.4 })
    if (themeFuse.search(search).length > 0) {
      actions.push('toggle-theme')
    }

    return {
      navigation: groupedNavResults.navigation,
      settings: groupedNavResults.settings,
      account: groupedNavResults.account,
      projectNav: groupedNavResults.projectNav,
      projects: filteredProjects,
      actions: actions,
    }
  }, [
    search,
    navFuse,
    projectsFuse,
    projects,
    currentProjectSlug,
    currentProject,
  ])

  return (
    <CommandDialog open={open} onOpenChange={setOpen}>
      <Command className="rounded-lg border shadow-md" loop>
        <CommandInput
          placeholder="Type a command or search..."
          value={search}
          onValueChange={setSearch}
        />
        <CommandList>
          <CommandEmpty>No results found.</CommandEmpty>

          {/* Project Navigation (shown first when on a project page) */}
          {searchResults.projectNav.length > 0 && currentProject && (
            <>
              <CommandGroup heading={`${currentProject.name}`}>
                {searchResults.projectNav.map((item) => (
                  <CommandItem
                    key={item.url}
                    onSelect={() => runCommand(() => navigate(item.url))}
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
                    onSelect={() => runCommand(() => navigate(item.url))}
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
                    onSelect={() => runCommand(() => navigate(item.url))}
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
                    onSelect={() => runCommand(() => navigate(item.url))}
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

          {/* Projects */}
          {searchResults.projects.length > 0 && (
            <>
              <CommandGroup heading="Projects">
                {searchResults.projects.map((project) => (
                  <CommandItem
                    key={project.id}
                    onSelect={() =>
                      runCommand(() => navigate(`/projects/${project.slug}`))
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
                  runCommand(() => document.body.classList.toggle('dark'))
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
