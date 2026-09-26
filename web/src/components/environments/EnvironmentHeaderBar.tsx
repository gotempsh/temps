// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import {
  sleepEnvironmentMutation,
  wakeEnvironmentMutation,
  getDeploymentOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Activity,
  Clock,
  ExternalLink,
  GitBranch,
  Loader2,
  Moon,
  Play,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'

interface EnvironmentHeaderBarProps {
  environment: EnvironmentResponse
  project: ProjectResponse
}

export function EnvironmentHeaderBar({
  environment,
  project,
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

  const statusTone = isSleeping
    ? 'bg-amber-50 text-amber-700 ring-amber-600/20 dark:bg-amber-500/10 dark:text-amber-400 dark:ring-amber-500/30'
    : environment.current_deployment_id
      ? 'bg-emerald-50 text-emerald-700 ring-emerald-600/20 dark:bg-emerald-500/10 dark:text-emerald-400 dark:ring-emerald-500/30'
      : 'bg-muted text-muted-foreground ring-border'

  return (
    <div className="border-b bg-background">
      <div className="w-full px-4 sm:px-6 lg:px-8">
        {/* Primary row */}
        <div className="flex flex-col gap-4 py-5 sm:flex-row sm:items-start sm:justify-between">
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2.5">
              <h1 className="truncate text-xl font-semibold tracking-tight text-foreground">
                {environment.name}
              </h1>
              <span
                className={`inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium ring-1 ring-inset ${statusTone}`}
              >
                <span
                  className={`size-1.5 rounded-full ${
                    isSleeping
                      ? 'bg-amber-500'
                      : environment.current_deployment_id
                        ? 'bg-emerald-500'
                        : 'bg-muted-foreground'
                  }`}
                  aria-hidden="true"
                />
                {isSleeping
                  ? 'Sleeping'
                  : environment.current_deployment_id
                    ? 'Running'
                    : 'Not deployed'}
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
          </div>
        </div>
      </div>
    </div>
  )
}
