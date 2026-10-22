// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2, ShieldCheck } from 'lucide-react'
import { toast } from 'sonner'
import { Label } from '@/components/ui/label'
import { SettingsSection } from '@/components/ui/settings-section'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { problemDetail } from '@/lib/api-problem'
import { cn } from '@/lib/utils'
import {
  getIngressSettings,
  ingressSettingsQueryKey,
  ingressStatusSummary,
  updateIngressSettings,
  type IngressMode,
  type IngressSettingsUpdate,
} from '@/lib/environment-ingress'

const MODES: { value: IngressMode; title: string; description: string }[] = [
  {
    value: 'legacy',
    title: 'Legacy',
    description: 'The existing access rules decide every request.',
  },
  {
    value: 'shadow',
    title: 'Shadow',
    description:
      'The existing rules still decide; the compiled endpoint runs alongside and every disagreement is counted in the proxy metrics.',
  },
  {
    value: 'enforce',
    title: 'Enforce',
    description:
      'The compiled endpoint decides. It applies the same IP, rate-limit, captcha and password rules, plus a Temps login when required.',
  },
]

const TITLE = 'Access Control'
const DESCRIPTION =
  "Choose how the proxy decides who may open this environment's URLs, and whether visitors must sign in to Temps."

/**
 * An environment's ingress mode and Temps login requirement (ADR-055 §11).
 */
export function EnvironmentIngressCard({
  projectId,
  environmentId,
}: {
  projectId: number
  environmentId: number
}) {
  const queryClient = useQueryClient()
  const queryKey = ingressSettingsQueryKey(projectId, environmentId)
  const settingsQuery = useQuery({
    queryKey,
    queryFn: () => getIngressSettings(projectId, environmentId),
    meta: { errorTitle: 'Could not load access control' },
  })
  const update = useMutation({
    mutationFn: (change: IngressSettingsUpdate) =>
      updateIngressSettings(projectId, environmentId, change),
    meta: { errorTitle: 'Could not update access control' },
    onSuccess: (settings) => {
      queryClient.setQueryData(queryKey, settings)
      toast.success('Access control updated')
    },
    onError: (error) => {
      toast.error(problemDetail(error, 'Could not update access control'))
    },
  })

  if (settingsQuery.isPending) {
    return (
      <SettingsSection
        title={TITLE}
        description={DESCRIPTION}
        icon={ShieldCheck}
      >
        <div className="space-y-3" aria-busy="true">
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-9 w-64" />
        </div>
      </SettingsSection>
    )
  }

  if (settingsQuery.isError) {
    return (
      <SettingsSection
        title={TITLE}
        description={DESCRIPTION}
        icon={ShieldCheck}
      >
        <ErrorAlert
          title="Could not load access control"
          description={problemDetail(
            settingsQuery.error,
            'The access control settings could not be loaded.'
          )}
          retry={() => settingsQuery.refetch()}
          retrying={settingsQuery.isFetching}
        />
      </SettingsSection>
    )
  }

  const settings = settingsQuery.data
  return (
    <SettingsSection title={TITLE} description={DESCRIPTION} icon={ShieldCheck}>
      <div className="space-y-5">
        <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
          {MODES.map((mode) => (
            <button
              key={mode.value}
              type="button"
              disabled={update.isPending}
              aria-pressed={settings.mode === mode.value}
              onClick={() => {
                if (settings.mode !== mode.value) {
                  update.mutate({ mode: mode.value })
                }
              }}
              className={cn(
                'rounded-lg border p-4 text-left transition-colors hover:bg-muted/50 disabled:opacity-60',
                settings.mode === mode.value && 'border-primary bg-primary/5'
              )}
            >
              <div className="font-medium">{mode.title}</div>
              <p className="mt-1 text-sm text-muted-foreground">
                {mode.description}
              </p>
            </button>
          ))}
        </div>

        <div className="flex items-start justify-between gap-4 rounded-lg border p-4">
          <div className="space-y-1">
            <Label htmlFor={`require-login-${environmentId}`}>
              Require a Temps login
            </Label>
            <p className="text-sm text-muted-foreground">
              Visitors are sent to this console to sign in, and only people who
              can view the project get in — for example, a staging site only
              your team should see.
            </p>
          </div>
          <div className="flex items-center gap-2">
            {update.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
            ) : null}
            <Switch
              id={`require-login-${environmentId}`}
              checked={settings.require_login}
              disabled={update.isPending}
              onCheckedChange={(checked) =>
                update.mutate({ require_login: checked })
              }
            />
          </div>
        </div>

        <p className="text-sm text-muted-foreground" role="status">
          {ingressStatusSummary(settings)}
        </p>
      </div>
    </SettingsSection>
  )
}
