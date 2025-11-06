import { ProjectResponse } from '@/api/client'
import { cn } from '@/lib/utils'
import {
  Activity,
  BarChart3,
  ChevronDown,
  ChevronRight,
  Database,
  FileText,
  GitBranch,
  Home,
  Monitor,
  ScrollText,
  Settings,
  Shield,
} from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { Link, useLocation, useNavigate } from 'react-router-dom'

// Keyboard shortcut component for Cmd/Ctrl modifier
interface CmdKeyboardShortcutProps {
  shortcut: string
  onTrigger: () => void
}

function CmdKeyboardShortcut({
  shortcut,
  onTrigger,
}: CmdKeyboardShortcutProps) {
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!e.metaKey && !e.ctrlKey) return
      if (e.key.toUpperCase() === shortcut.toUpperCase()) {
        e.preventDefault()
        onTrigger()
      }
    }

    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [shortcut, onTrigger])

  return null
}

interface ProjectDetailSidebarProps {
  project: ProjectResponse
}

interface NavItem {
  title: string
  url: string
  icon: any
  kbd?: string
  subItems?: { title: string; url: string }[]
}

const navItems: NavItem[] = [
  {
    title: 'Project',
    url: 'project',
    icon: Home,
    kbd: 'P',
  },
  {
    title: 'Deployments',
    url: 'deployments',
    icon: GitBranch,
    kbd: 'D',
  },
  {
    title: 'Analytics',
    url: 'analytics',
    icon: BarChart3,
    subItems: [
      { title: 'Overview', url: 'analytics' },
      { title: 'Visitors', url: 'analytics/visitors' },
      { title: 'Pages', url: 'analytics/pages' },
      { title: 'Replays', url: 'analytics/replays' },
      { title: 'Funnels', url: 'analytics/funnels' },
      { title: 'Speed Insights', url: 'speed' },
      { title: 'Setup', url: 'analytics/setup' },
    ],
  },
  {
    title: 'Request Logs',
    url: 'analytics/requests',
    icon: FileText,
  },
  {
    title: 'Storage',
    url: 'storage',
    icon: Database,
    kbd: 'S',
  },
  {
    title: 'Runtime Logs',
    url: 'runtime',
    icon: ScrollText,
    kbd: 'L',
  },
  {
    title: 'Error Tracking',
    url: 'errors',
    icon: Shield,
    kbd: 'E',
  },
  {
    title: 'Monitors',
    url: 'monitors',
    icon: Activity,
    kbd: 'M',
  },
  {
    title: 'Settings',
    url: 'settings',
    icon: Settings,
    kbd: ',',
    subItems: [
      { title: 'General', url: 'settings/general' },
      { title: 'Domains', url: 'settings/domains' },
      { title: 'Environments', url: 'settings/environments' },
      { title: 'Environment Variables', url: 'settings/environment-variables' },
      { title: 'Git', url: 'settings/git' },
      { title: 'Security', url: 'settings/security' },
      { title: 'Cron Jobs', url: 'settings/cron-jobs' },
    ],
  },
]

export function ProjectDetailSidebar({ project }: ProjectDetailSidebarProps) {
  const location = useLocation()
  const navigate = useNavigate()
  const [expandedItems, setExpandedItems] = useState<string[]>(['analytics'])

  const isActive = (url: string) => {
    const path = location.pathname
    if (url === 'project') {
      return path.endsWith('/project') || path.endsWith(`/${project.slug}`)
    }
    // For exact matching, check if the path ends with the url
    const pathParts = path.split('/')
    const urlParts = url.split('/')

    // Match the exact route structure
    if (pathParts.length !== urlParts.length + 3) return false // +3 for /projects/{slug}/

    return pathParts.slice(-urlParts.length).join('/') === url
  }

  const isParentActive = (item: NavItem) => {
    // Parent is only active if we're on the first sub-item (default route)
    if (!item.subItems || item.subItems.length === 0) return false
    const path = location.pathname
    // Check if we're on the parent route exactly (e.g., /projects/slug/analytics)
    return path.endsWith(`/${item.url}`) && !path.includes(`/${item.url}/`)
  }

  const toggleExpanded = useCallback(
    (title: string) => {
      setExpandedItems((prev) =>
        prev.includes(title)
          ? prev.filter((t) => t !== title)
          : [...prev, title]
      )
    },
    [setExpandedItems]
  )

  const handleNavigate = useCallback(
    (item: NavItem) => {
      // If item has sub-items, expand it first
      if (item.subItems && item.subItems.length > 0) {
        setExpandedItems((prev) => {
          // If not already expanded, add it to expanded items
          if (!prev.includes(item.title)) {
            return [...prev, item.title]
          }
          return prev
        })
      }

      // Navigate to the target URL
      const targetUrl = item.subItems ? item.subItems[0].url : item.url
      navigate(`/projects/${project.slug}/${targetUrl}`)
    },
    [project.slug, navigate]
  )

  return (
    <div className="hidden md:flex h-full w-56 flex-col border-r bg-background overflow-hidden">
      {/* Keyboard shortcuts */}
      {navItems.map(
        (item) =>
          item.kbd && (
            <CmdKeyboardShortcut
              key={item.kbd}
              shortcut={item.kbd}
              onTrigger={() => handleNavigate(item)}
            />
          )
      )}

      <nav className="flex flex-col gap-1 p-2 overflow-y-auto">
        {navItems.map((item) => {
          const Icon = item.icon
          const active = isActive(item.url)
          const hasSubItems = item.subItems && item.subItems.length > 0
          const isExpanded = expandedItems.includes(item.title)
          const parentActive = isParentActive(item)

          return (
            <div key={item.title}>
              {hasSubItems ? (
                <>
                  <div className="flex items-center gap-0">
                    <Link
                      to={`/projects/${project.slug}/${item.subItems[0].url}`}
                      className={cn(
                        'flex flex-1 items-center gap-3 rounded-l-lg px-3 py-2 text-sm transition-all hover:bg-accent',
                        active || parentActive
                          ? 'bg-accent text-accent-foreground font-medium'
                          : 'text-muted-foreground'
                      )}
                    >
                      <Icon className="h-4 w-4" />
                      <span className="flex-1 text-left">{item.title}</span>
                      {item.kbd && (
                        <kbd className="pointer-events-none hidden h-5 select-none items-center gap-1 rounded border bg-muted px-1.5 font-mono text-[10px] font-medium opacity-100 sm:flex">
                          <span className="text-xs">⌘</span>
                          {item.kbd}
                        </kbd>
                      )}
                    </Link>
                    <button
                      onClick={() => toggleExpanded(item.title)}
                      className={cn(
                        'flex items-center justify-center rounded-r-lg px-2 py-2 text-sm transition-all hover:bg-accent',
                        active || parentActive
                          ? 'bg-accent text-accent-foreground font-medium'
                          : 'text-muted-foreground'
                      )}
                    >
                      {isExpanded ? (
                        <ChevronDown className="h-4 w-4" />
                      ) : (
                        <ChevronRight className="h-4 w-4" />
                      )}
                    </button>
                  </div>
                  {isExpanded && (
                    <div className="ml-7 mt-1 flex flex-col gap-1">
                      {item.subItems.map((subItem) => {
                        const subActive = isActive(subItem.url)
                        return (
                          <Link
                            key={subItem.url}
                            to={`/projects/${project.slug}/${subItem.url}`}
                            className={cn(
                              'rounded-lg px-3 py-1.5 text-sm transition-all hover:bg-accent',
                              subActive
                                ? 'bg-accent text-accent-foreground font-medium'
                                : 'text-muted-foreground'
                            )}
                          >
                            {subItem.title}
                          </Link>
                        )
                      })}
                    </div>
                  )}
                </>
              ) : (
                <Link
                  to={`/projects/${project.slug}/${item.url}`}
                  className={cn(
                    'flex items-center gap-3 rounded-lg px-3 py-2 text-sm transition-all hover:bg-accent',
                    active
                      ? 'bg-accent text-accent-foreground font-medium'
                      : 'text-muted-foreground'
                  )}
                >
                  <Icon className="h-4 w-4" />
                  <span className="flex-1">{item.title}</span>
                  {item.kbd && (
                    <kbd className="pointer-events-none hidden h-5 select-none items-center gap-1 rounded border bg-muted px-1.5 font-mono text-[10px] font-medium opacity-100 sm:flex">
                      <span className="text-xs">⌘</span>
                      {item.kbd}
                    </kbd>
                  )}
                </Link>
              )}
            </div>
          )
        })}
      </nav>
    </div>
  )
}
