import { EnvironmentResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { Box, Settings } from 'lucide-react'
import { useCallback } from 'react'

interface EnvironmentSidebarProps {
  environment: EnvironmentResponse
  activeView: string
  onViewChange: (view: string) => void
  isStatic: boolean
}

export interface NavItem {
  title: string
  view: string
  icon: React.ComponentType<{ className?: string }>
  visible?: boolean
  shortcut?: string
}

export function EnvironmentSidebar({
  environment,
  activeView,
  onViewChange,
  isStatic,
}: EnvironmentSidebarProps) {
  const navItems: NavItem[] = [
    {
      title: 'Containers',
      view: 'containers',
      icon: Box,
      visible: !isStatic,
      shortcut: '⌘C',
    },
    {
      title: 'Settings',
      view: 'settings',
      icon: Settings,
      visible: true,
      shortcut: '⌘,',
    },
  ]

  const visibleItems = navItems.filter((item) => item.visible !== false)

  const handleNavClick = useCallback(
    (view: string) => {
      onViewChange(view)
    },
    [onViewChange]
  )

  return (
    <>
      {/* Mobile Navigation - Tabs */}
      <div className="lg:hidden border-b bg-background">
        <Tabs value={activeView} onValueChange={handleNavClick}>
          <TabsList className="w-full rounded-none bg-transparent border-b-0 justify-start h-auto p-0">
            {visibleItems.map((item) => {
              const Icon = item.icon
              return (
                <TabsTrigger
                  key={item.view}
                  value={item.view}
                  className="rounded-none border-b-2 border-b-transparent data-[state=active]:border-b-primary data-[state=active]:bg-transparent"
                >
                  <Icon className="h-4 w-4 mr-2" />
                  <span className="text-sm">{item.title}</span>
                </TabsTrigger>
              )
            })}
          </TabsList>
        </Tabs>
      </div>

      {/* Desktop Navigation - Sidebar */}
      <div className="hidden lg:flex w-64 border-r bg-muted/30 flex-col h-full">
        {/* Environment Info */}
        <div className="p-4 border-b">
          <div className="space-y-1">
            <h3 className="font-semibold text-sm truncate">
              {environment.name}
            </h3>
            {environment.branch && (
              <p className="text-xs text-muted-foreground truncate">
                Branch: {environment.branch}
              </p>
            )}
          </div>
        </div>

        {/* Navigation */}
        <nav className="flex-1 p-2 overflow-y-auto space-y-1">
          <TooltipProvider>
            {visibleItems.map((item) => {
              const Icon = item.icon
              const isActive = activeView === item.view

              return (
                <Tooltip key={item.view}>
                  <TooltipTrigger asChild>
                    <Button
                      variant={isActive ? 'secondary' : 'ghost'}
                      size="sm"
                      className="w-full justify-start gap-2 h-9"
                      onClick={() => handleNavClick(item.view)}
                    >
                      <Icon className="h-4 w-4 flex-shrink-0" />
                      <span className="truncate">{item.title}</span>
                    </Button>
                  </TooltipTrigger>
                  {item.shortcut && (
                    <TooltipContent
                      side="right"
                      className="flex items-center gap-2"
                    >
                      <span>{item.title}</span>
                      <kbd className="hidden sm:inline-flex items-center gap-1 rounded border border-border bg-muted px-1.5 py-0.5 text-xs font-medium text-muted-foreground">
                        {item.shortcut}
                      </kbd>
                    </TooltipContent>
                  )}
                </Tooltip>
              )
            })}
          </TooltipProvider>
        </nav>

        {/* Environment Details Footer */}
        <div className="border-t p-3 text-xs text-muted-foreground space-y-1">
          <div>
            <span className="font-medium">ID:</span>{' '}
            <span className="font-mono text-xs">{environment.id}</span>
          </div>
          {environment.branch && (
            <div>
              <span className="font-medium">Branch:</span>{' '}
              <span className="font-mono text-xs">{environment.branch}</span>
            </div>
          )}
        </div>
      </div>
    </>
  )
}
