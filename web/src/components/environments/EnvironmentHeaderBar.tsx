import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import {
  sleepEnvironmentMutation,
  wakeEnvironmentMutation,
  getDeploymentOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Activity,
  Check,
  ChevronsUpDown,
  Clock,
  ExternalLink,
  GitBranch,
  Loader2,
  Moon,
  Play,
  Plus,
  Settings,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { toast } from 'sonner'

interface EnvironmentHeaderBarProps {
  environment: EnvironmentResponse
  project: ProjectResponse
  activeView: string
  onViewChange: (view: string) => void
  environments?: EnvironmentResponse[]
  onEnvironmentChange?: (id: number) => void
  onCreateEnvironment?: () => void
}

export function EnvironmentHeaderBar({
  environment,
  project,
  activeView,
  onViewChange,
  environments,
  onEnvironmentChange,
  onCreateEnvironment,
}: EnvironmentHeaderBarProps) {
  const queryClient = useQueryClient()
  const isOnDemand = environment.deployment_config?.onDemand ?? false
  const isSleeping = Boolean(environment.sleeping)

  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!isOnDemand || isSleeping || !environment.estimated_sleep_at) return
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [isOnDemand, isSleeping, environment.estimated_sleep_at])

  const sleepCountdown = useMemo(() => {
    if (!environment.estimated_sleep_at || isSleeping) return null
    const remaining = Math.max(
      0,
      Math.floor((environment.estimated_sleep_at - now) / 1000)
    )
    if (remaining <= 0) return 'any moment'
    const minutes = Math.floor(remaining / 60)
    const seconds = remaining % 60
    if (minutes > 0) return `${minutes}m ${seconds}s`
    return `${seconds}s`
  }, [environment.estimated_sleep_at, isSleeping, now])

  const lastActivityLabel = useMemo(() => {
    if (!environment.last_activity_at) return null
    const ago = Math.floor((now - environment.last_activity_at) / 1000)
    if (ago < 5) return 'just now'
    if (ago < 60) return `${ago}s ago`
    const minutes = Math.floor(ago / 60)
    if (minutes < 60) return `${minutes}m ago`
    const hours = Math.floor(minutes / 60)
    if (hours < 24) return `${hours}h ago`
    return `${Math.floor(hours / 24)}d ago`
  }, [environment.last_activity_at, now])

  const wakeMutation = useMutation({
    ...wakeEnvironmentMutation(),
    onSuccess: () => {
      toast.success('Environment is waking up')
      queryClient.invalidateQueries({ queryKey: ['environment'] })
    },
    meta: { errorTitle: 'Failed to wake environment' },
  })

  const sleepMutation = useMutation({
    ...sleepEnvironmentMutation(),
    onSuccess: () => {
      toast.success('Environment is going to sleep')
      queryClient.invalidateQueries({ queryKey: ['environment'] })
    },
    meta: { errorTitle: 'Failed to sleep environment' },
  })

  const { data: deployment } = useQuery({
    ...getDeploymentOptions({
      path: {
        project_id: project.id,
        deployment_id: environment.current_deployment_id ?? 0,
      },
    }),
    enabled: !!environment.current_deployment_id,
  })

  const inSettings = activeView === 'settings'

  const statusTone = isSleeping
    ? 'bg-amber-50 text-amber-700 ring-amber-600/20 dark:bg-amber-500/10 dark:text-amber-400 dark:ring-amber-500/30'
    : 'bg-emerald-50 text-emerald-700 ring-emerald-600/20 dark:bg-emerald-500/10 dark:text-emerald-400 dark:ring-emerald-500/30'

  const hasMultipleEnvs = (environments?.length ?? 0) > 1
  const canSwitchEnvs = !!environments && environments.length > 0

  return (
    <div className="sticky top-0 z-10 bg-white/95 backdrop-blur dark:bg-neutral-950/95">
      <div className="w-full px-4 sm:px-6 lg:px-8">
        {/* Primary row */}
        <div className="flex flex-col gap-4 py-5 sm:flex-row sm:items-start sm:justify-between">
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2.5">
              {canSwitchEnvs && hasMultipleEnvs ? (
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <button
                      type="button"
                      className="group inline-flex items-center gap-1.5 rounded-md px-1.5 py-0.5 -ml-1.5 text-2xl font-semibold tracking-tight text-neutral-950 hover:bg-neutral-100 dark:text-white dark:hover:bg-white/5"
                    >
                      <span className="truncate">{environment.name}</span>
                      <ChevronsUpDown
                        className="size-4 text-neutral-400 group-hover:text-neutral-600 dark:group-hover:text-neutral-300"
                        aria-hidden="true"
                      />
                    </button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="start" className="w-64">
                    {environments!.map((env) => (
                      <DropdownMenuItem
                        key={env.id}
                        onSelect={() => onEnvironmentChange?.(env.id)}
                        className="flex items-start gap-2"
                      >
                        <Check
                          className={`size-4 mt-0.5 shrink-0 ${
                            env.id === environment.id
                              ? 'opacity-100'
                              : 'opacity-0'
                          }`}
                          aria-hidden="true"
                        />
                        <div className="flex flex-col min-w-0">
                          <span className="font-medium truncate">
                            {env.name}
                          </span>
                          {env.branch && (
                            <span className="text-xs text-neutral-500 dark:text-neutral-400 font-mono truncate">
                              {env.branch}
                            </span>
                          )}
                        </div>
                      </DropdownMenuItem>
                    ))}
                    {onCreateEnvironment && (
                      <>
                        <DropdownMenuSeparator />
                        <DropdownMenuItem onSelect={onCreateEnvironment}>
                          <Plus className="size-4 mr-2" aria-hidden="true" />
                          New environment
                        </DropdownMenuItem>
                      </>
                    )}
                  </DropdownMenuContent>
                </DropdownMenu>
              ) : (
                <>
                  <h1 className="truncate text-2xl font-semibold tracking-tight text-neutral-950 dark:text-white">
                    {environment.name}
                  </h1>
                  {onCreateEnvironment && (
                    <button
                      type="button"
                      onClick={onCreateEnvironment}
                      className="inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-sm text-neutral-500 hover:bg-neutral-100 hover:text-neutral-700 dark:text-neutral-400 dark:hover:bg-white/5 dark:hover:text-neutral-200"
                    >
                      <Plus className="size-3.5" aria-hidden="true" />
                      New environment
                    </button>
                  )}
                </>
              )}
              <span
                className={`inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium ring-1 ring-inset ${statusTone}`}
              >
                <span
                  className={`size-1.5 rounded-full ${
                    isSleeping ? 'bg-amber-500' : 'bg-emerald-500'
                  }`}
                  aria-hidden="true"
                />
                {isSleeping ? 'Sleeping' : 'Running'}
              </span>
              {environment.slug === 'production' && (
                <span className="inline-flex items-center rounded-full bg-neutral-100 px-2 py-0.5 text-xs font-medium text-neutral-700 ring-1 ring-inset ring-neutral-950/10 dark:bg-white/5 dark:text-neutral-300 dark:ring-white/10">
                  Production
                </span>
              )}
            </div>
            <div className="mt-2 flex flex-wrap items-center gap-x-5 gap-y-1.5 text-sm text-neutral-600 dark:text-neutral-400">
              {environment.branch && (
                <div className="inline-flex items-center gap-1.5">
                  <GitBranch className="size-3.5" aria-hidden="true" />
                  <code className="font-mono text-[0.8125rem]">
                    {environment.branch}
                  </code>
                </div>
              )}
              {isOnDemand && lastActivityLabel && !isSleeping && (
                <div className="inline-flex items-center gap-1.5">
                  <Activity className="size-3.5" aria-hidden="true" />
                  <span>Last active {lastActivityLabel}</span>
                </div>
              )}
              {isOnDemand && sleepCountdown && !isSleeping && (
                <div className="inline-flex items-center gap-1.5 tabular-nums">
                  <Clock className="size-3.5" aria-hidden="true" />
                  <span>Sleeps in {sleepCountdown}</span>
                </div>
              )}
              {deployment && (
                <Link
                  to={`/projects/${project.slug}/deployments/${deployment.id}`}
                  className="inline-flex items-center gap-1.5 text-neutral-900 hover:underline dark:text-white"
                >
                  <span
                    className={`size-1.5 rounded-full ${
                      deployment.status === 'completed'
                        ? 'bg-emerald-500'
                        : deployment.status === 'failed'
                          ? 'bg-red-500'
                          : 'bg-neutral-400'
                    }`}
                    aria-hidden="true"
                  />
                  <span className="capitalize">{deployment.status}</span>
                  <span className="text-neutral-500 dark:text-neutral-400">
                    deployment
                  </span>
                  <ExternalLink className="size-3" aria-hidden="true" />
                </Link>
              )}
            </div>
          </div>

          <div className="flex items-center gap-2">
            {inSettings ? (
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => onViewChange('containers')}
              >
                Done
              </Button>
            ) : (
              <>
                {isOnDemand &&
                  (isSleeping ? (
                    <Button
                      type="button"
                      size="sm"
                      disabled={wakeMutation.isPending}
                      onClick={() =>
                        wakeMutation.mutate({
                          path: {
                            project_id: environment.project_id,
                            env_id: environment.id,
                          },
                        })
                      }
                    >
                      {wakeMutation.isPending ? (
                        <Loader2 className="mr-1.5 size-4 animate-spin" />
                      ) : (
                        <Play className="mr-1.5 size-4" />
                      )}
                      Wake up
                    </Button>
                  ) : (
                    <TooltipProvider>
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Button
                            type="button"
                            variant="outline"
                            size="sm"
                            disabled={sleepMutation.isPending}
                            onClick={() =>
                              sleepMutation.mutate({
                                path: {
                                  project_id: environment.project_id,
                                  env_id: environment.id,
                                },
                              })
                            }
                          >
                            {sleepMutation.isPending ? (
                              <Loader2 className="mr-1.5 size-4 animate-spin" />
                            ) : (
                              <Moon className="mr-1.5 size-4" />
                            )}
                            Sleep now
                          </Button>
                        </TooltipTrigger>
                        <TooltipContent>
                          Put this environment to sleep
                        </TooltipContent>
                      </Tooltip>
                    </TooltipProvider>
                  ))}
                <TooltipProvider>
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <Button
                        type="button"
                        variant="outline"
                        size="icon"
                        className="size-9"
                        aria-label="Environment settings"
                        onClick={() => onViewChange('settings')}
                      >
                        <Settings className="size-4" aria-hidden="true" />
                      </Button>
                    </TooltipTrigger>
                    <TooltipContent>Environment settings</TooltipContent>
                  </Tooltip>
                </TooltipProvider>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}
