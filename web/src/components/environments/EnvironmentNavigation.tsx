// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EnvironmentResponse } from '@/api/client'
import type { EnvironmentView } from '@/lib/environment-navigation'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Check,
  Boxes,
  ChevronsUpDown,
  LineChart,
  Plus,
  Settings2,
} from 'lucide-react'

const views = [
  { value: 'containers', label: 'Containers', icon: Boxes },
  { value: 'metrics', label: 'Metrics', icon: LineChart },
  { value: 'settings', label: 'Settings', icon: Settings2 },
] as const

export function EnvironmentNavigation({
  environment,
  environments,
  activeView,
  onViewChange,
  onEnvironmentChange,
  onCreateEnvironment,
}: {
  environment: EnvironmentResponse
  environments?: EnvironmentResponse[]
  activeView: EnvironmentView
  onViewChange: (view: EnvironmentView) => void
  onEnvironmentChange?: (id: number) => void
  onCreateEnvironment?: () => void
}) {
  const available = environments?.length ? environments : [environment]
  const currentView =
    views.find((view) => view.value === activeView) ?? views[0]
  const CurrentIcon = currentView.icon
  return (
    <aside
      aria-label="Environment navigation"
      className="border-b p-4 lg:border-b-0 lg:border-r lg:px-3 lg:py-5"
    >
      <div className="flex min-w-0 items-center gap-2 lg:sticky lg:top-5 lg:block">
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="outline"
              aria-label={`Switch environment: ${environment.name}`}
              className="h-10 min-w-0 flex-1 justify-between gap-2 px-3 py-2.5 lg:w-full"
            >
              <span className="flex min-w-0 items-center gap-2">
                <EnvironmentStatusDot environment={environment} />
                <span className="truncate text-sm font-medium">
                  {environment.name}
                </span>
              </span>
              <ChevronsUpDown className="size-3.5 shrink-0 text-muted-foreground" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-60">
            <DropdownMenuRadioGroup
              value={String(environment.id)}
              onValueChange={(id) => onEnvironmentChange?.(Number(id))}
            >
              {available.map((env) => (
                <DropdownMenuRadioItem
                  key={env.id}
                  indicator={<Check className="size-4" aria-hidden="true" />}
                  value={String(env.id)}
                  disabled={!onEnvironmentChange && env.id !== environment.id}
                >
                  <span className="min-w-0">
                    <span className="flex items-center gap-2">
                      <EnvironmentStatusDot environment={env} />
                      <span className="truncate">{env.name}</span>
                    </span>
                    {env.branch && (
                      <span className="block truncate font-mono text-xs text-muted-foreground">
                        {env.branch}
                      </span>
                    )}
                  </span>
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
            {onCreateEnvironment && (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuItem onSelect={onCreateEnvironment}>
                  <Plus className="mr-2 size-4" />
                  Create environment
                </DropdownMenuItem>
              </>
            )}
          </DropdownMenuContent>
        </DropdownMenu>

        <nav
          aria-label="Environment pages"
          className="mt-5 hidden space-y-0.5 lg:block"
        >
          {views.map(({ value, label, icon: Icon }) => (
            <button
              key={value}
              type="button"
              aria-current={activeView === value ? 'page' : undefined}
              onClick={() => onViewChange(value)}
              className={cn(
                'flex w-full items-center gap-2 rounded-md px-3 py-2 text-sm transition-colors',
                activeView === value
                  ? 'bg-accent font-medium text-accent-foreground'
                  : 'text-muted-foreground hover:bg-accent/60 hover:text-foreground'
              )}
            >
              <Icon className="size-4 shrink-0" aria-hidden="true" />
              {label}
            </button>
          ))}
        </nav>
        <div className="lg:hidden">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="outline"
                aria-label={`Environment page: ${currentView.label}`}
                className="h-10 gap-2"
              >
                <CurrentIcon className="size-4" />
                {currentView.label}
                <ChevronsUpDown className="size-3.5" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              {views.map(({ value, label, icon: Icon }) => (
                <DropdownMenuItem
                  key={value}
                  onSelect={() => onViewChange(value)}
                >
                  <Icon className="mr-2 size-4" />
                  {label}
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>
    </aside>
  )
}

function EnvironmentStatusDot({
  environment,
}: {
  environment: EnvironmentResponse
}) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        'size-1.5 shrink-0 rounded-full',
        environment.sleeping
          ? 'bg-amber-500'
          : environment.current_deployment_id
            ? 'bg-emerald-500'
            : 'bg-muted-foreground'
      )}
    />
  )
}
