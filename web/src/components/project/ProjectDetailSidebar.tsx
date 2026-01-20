import { ProjectResponse } from '@/api/client'
import { useAuth } from '@/contexts/AuthContext'
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
  Layers,
  ScrollText,
  Settings,
  Shield,
  ShieldAlert,
  Key,
  Globe,
  Boxes,
} from 'lucide-react'
import {
  useCallback,
  useContext,
  useEffect,
  useState,
  createContext,
} from 'react'
import { Link, useLocation, useNavigate } from 'react-router-dom'
import { Sheet, SheetContent } from '@/components/ui/sheet'

// Context for mobile sidebar menu state
interface MobileSidebarContextType {
  isOpen: boolean
  setIsOpen: (open: boolean) => void
}

const MobileSidebarContext = createContext<
  MobileSidebarContextType | undefined
>(undefined)

export function useMobileSidebar() {
  const context = useContext(MobileSidebarContext)
  if (!context) {
    throw new Error(
      'useMobileSidebar must be used within a ProjectDetailSidebar'
    )
  }
  return context
}

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

const baseNavItems: NavItem[] = [
  // Core Development
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

  // Monitoring & Analytics (High Frequency)
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
    title: 'Error Tracking',
    url: 'errors',
    icon: ShieldAlert,
    kbd: 'E',
  },
  {
    title: 'Security',
    url: 'security',
    icon: Shield,
  },
  {
    title: 'Monitors',
    url: 'monitors',
    icon: Activity,
    kbd: 'M',
  },

  // Debugging (Medium Frequency)
  {
    title: 'Runtime Logs',
    url: 'runtime',
    icon: ScrollText,
    kbd: 'L',
  },
  {
    title: 'HTTP Requests',
    url: 'analytics/requests',
    icon: FileText,
  },

  // Management (Medium Frequency)
  {
    title: 'Domains',
    url: 'settings/domains',
    icon: Globe,
  },
  {
    title: 'Storage',
    url: 'storage',
    icon: Database,
    kbd: 'S',
  },
  {
    title: 'Services',
    url: 'services',
    icon: Boxes,
    subItems: [
      { title: 'Overview', url: 'services' },
      { title: 'KV Store', url: 'services/kv' },
      { title: 'Blob Storage', url: 'services/blob' },
    ],
  },

  // Configuration (Lower Frequency)
  {
    title: 'Environment Variables',
    url: 'settings/environment-variables',
    icon: Key,
  },
  {
    title: 'Git',
    url: 'settings/git',
    icon: GitBranch,
  },
  {
    title: 'Settings',
    url: 'settings',
    icon: Settings,
    kbd: ',',
    subItems: [
      { title: 'General', url: 'settings/general' },
      { title: 'Security', url: 'settings/security' },
      { title: 'Cron Jobs', url: 'settings/cron-jobs' },
      { title: 'Webhooks', url: 'settings/webhooks' },
    ],
  },
]

interface MobileSidebarProviderProps {
  children: React.ReactNode
}

export function MobileSidebarProvider({
  children,
}: MobileSidebarProviderProps) {
  const [isMobileMenuOpen, setIsMobileMenuOpen] = useState(false)

  return (
    <MobileSidebarContext.Provider
      value={{ isOpen: isMobileMenuOpen, setIsOpen: setIsMobileMenuOpen }}
    >
      {children}
    </MobileSidebarContext.Provider>
  )
}

export function ProjectDetailSidebar({ project }: ProjectDetailSidebarProps) {
  const location = useLocation()
  const navigate = useNavigate()
  const { isDemoMode } = useAuth()
  const [expandedItems, setExpandedItems] = useState<string[]>([
    'analytics',
    'settings',
  ])

  // Build nav items including environments
  const settingsIndex = baseNavItems.length - 1
  const allNavItems: NavItem[] = [
    ...baseNavItems.slice(0, settingsIndex),
    {
      title: 'Environments',
      url: 'environments',
      icon: Layers,
    },
    baseNavItems[settingsIndex],
  ]

  // In demo mode, only show Analytics and Monitors as flat items (no sub-items)
  // This creates a simplified view for the demo
  const demoNavItems: NavItem[] = [
    {
      title: 'Analytics',
      url: 'analytics',
      icon: BarChart3,
    },
    {
      title: 'Monitors',
      url: 'monitors',
      icon: Activity,
    },
  ]
  const navItems = isDemoMode ? demoNavItems : allNavItems

  // Auto-expand parent items when navigating to their sub-items
  useEffect(() => {
    const path = location.pathname
    navItems.forEach((item) => {
      if (item.subItems) {
        const isOnSubItem = item.subItems.some((subItem) =>
          path.includes(`/${subItem.url}`)
        )
        if (isOnSubItem && !expandedItems.includes(item.title)) {
          setExpandedItems((prev) => [...prev, item.title])
        }
      }
    })
  }, [location.pathname])

  const isActive = (url: string) => {
    const path = location.pathname
    if (url === 'project') {
      return path.endsWith('/project') || path.endsWith(`/${project.slug}`)
    }
    // For exact matching, check if the path ends with the url
    const pathParts = path.split('/')
    const urlParts = url.split('/')

    // Match the exact route structure - account for variable length paths
    const projectSlugIndex = pathParts.indexOf(project.slug)
    if (projectSlugIndex === -1) return false

    const routeParts = pathParts.slice(projectSlugIndex + 1)

    // For environments/{id}, check if it starts with environments
    if (url === 'environments') {
      return routeParts[0] === 'environments'
    }

    // For exact matching
    if (routeParts.length !== urlParts.length) return false
    return routeParts.join('/') === url
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

  // Navigation content component (reusable for mobile and desktop)
  const NavigationContent = ({ closeSheet }: { closeSheet?: () => void }) => (
    <nav className="flex flex-col gap-1 p-2 overflow-y-auto">
      {navItems.map((item) => {
        const Icon = item.icon
        const active = isActive(item.url)
        const hasSubItems = item.subItems && item.subItems.length > 0
        const isExpanded = expandedItems.includes(item.title)
        const parentActive = isParentActive(item)

        return (
          <div key={item.title}>
            {hasSubItems && item.subItems ? (
              <>
                <div className="flex items-center gap-0">
                  <Link
                    to={`/projects/${project.slug}/${item.subItems[0].url}`}
                    onClick={closeSheet}
                    className={cn(
                      'flex flex-1 items-center gap-3 rounded-l-lg px-3 py-2 text-sm transition-all hover:bg-accent',
                      active || parentActive
                        ? 'bg-accent text-accent-foreground font-medium'
                        : 'text-muted-foreground'
                    )}
                  >
                    <Icon className="h-4 w-4 flex-shrink-0" />
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
                          onClick={closeSheet}
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
                onClick={closeSheet}
                className={cn(
                  'flex items-center gap-3 rounded-lg px-3 py-2 text-sm transition-all hover:bg-accent',
                  active
                    ? 'bg-accent text-accent-foreground font-medium'
                    : 'text-muted-foreground'
                )}
              >
                <Icon className="h-4 w-4 flex-shrink-0" />
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
  )

  const { isOpen, setIsOpen } = useMobileSidebar()

  return (
    <>
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

      {/* Desktop sidebar - hidden on mobile */}
      <div className="hidden md:flex h-full w-56 flex-col border-r bg-background overflow-hidden">
        <NavigationContent />
      </div>

      {/* Mobile menu sheet - controlled by context */}
      <Sheet open={isOpen} onOpenChange={setIsOpen}>
        <SheetContent side="left" className="w-72 p-0">
          <div className="h-full flex flex-col overflow-hidden">
            <div className="border-b p-4">
              <h2 className="font-semibold text-lg">{project.name}</h2>
            </div>
            <NavigationContent closeSheet={() => setIsOpen(false)} />
          </div>
        </SheetContent>
      </Sheet>
    </>
  )
}
