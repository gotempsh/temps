// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createNotificationRouteMutation,
  getNotificationRouteOptions,
  listNotificationProvidersOptions,
  listNotificationRoutesQueryKey,
  updateNotificationRouteMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { NotificationProviderResponse } from '@/api/client/types.gen'
import {
  configuredSlackChannel,
  severities,
  severityLabels,
  severityRangeLabel,
} from '@/components/monitoring/notificationRouteUtils'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, BellRing, Route as RouteIcon } from 'lucide-react'
import { FormEvent, useEffect, useMemo, useState } from 'react'
import { useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'

type RouteDraft = {
  name: string
  enabled: boolean
  min_severity: string
  max_severity: string
  provider_ids: number[]
}

const emptyDraft = (): RouteDraft => ({
  name: '',
  enabled: true,
  min_severity: 'debug',
  max_severity: 'emergency',
  provider_ids: [],
})

export function NotificationRouteForm() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const routeId = Number.parseInt(id || '0', 10)
  const isEditing = Boolean(id)

  usePageTitle(
    isEditing ? 'Edit Notification Route' : 'Create Notification Route'
  )

  const { data: providers, isLoading: providersLoading } = useQuery({
    ...listNotificationProvidersOptions({ query: { page_size: 100 } }),
  })
  const {
    data: route,
    isLoading: routeLoading,
    isError: routeError,
  } = useQuery({
    ...getNotificationRouteOptions({ path: { id: routeId } }),
    enabled: isEditing && Number.isInteger(routeId) && routeId > 0,
  })
  const [draftChanges, setDraftChanges] = useState<Partial<RouteDraft>>({})
  const baseDraft = useMemo<RouteDraft>(
    () =>
      route
        ? {
            name: route.name,
            enabled: route.enabled,
            min_severity: route.min_severity,
            max_severity: route.max_severity,
            provider_ids: route.provider_ids,
          }
        : emptyDraft(),
    [route]
  )
  const draft = useMemo(
    () => ({ ...baseDraft, ...draftChanges }),
    [baseDraft, draftChanges]
  )
  const setDraft = (update: (current: RouteDraft) => RouteDraft) => {
    setDraftChanges((current) => update({ ...baseDraft, ...current }))
  }

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      {
        label: 'Notification Routes',
        href: '/settings/notifications?tab=routes',
      },
      { label: isEditing ? route?.name || 'Edit Route' : 'Create Route' },
    ])
  }, [isEditing, route?.name, setBreadcrumbs])

  const providerById = useMemo(
    () =>
      new Map(
        (providers || []).map((provider: NotificationProviderResponse) => [
          provider.id,
          provider,
        ])
      ),
    [providers]
  )

  const returnToRoutes = () => navigate('/settings/notifications?tab=routes')
  const onSaved = async (message: string) => {
    await queryClient.invalidateQueries({
      queryKey: listNotificationRoutesQueryKey(),
    })
    toast.success(message)
    returnToRoutes()
  }

  const createMutation = useMutation({
    ...createNotificationRouteMutation(),
    meta: { errorTitle: 'Failed to create notification route' },
    onSuccess: () => onSaved('Notification route created'),
  })
  const updateMutation = useMutation({
    ...updateNotificationRouteMutation(),
    meta: { errorTitle: 'Failed to update notification route' },
    onSuccess: () => onSaved('Notification route updated'),
  })

  const toggleProvider = (providerId: number, checked: boolean) => {
    setDraft((current) => ({
      ...current,
      provider_ids: checked
        ? [...current.provider_ids, providerId]
        : current.provider_ids.filter((candidate) => candidate !== providerId),
    }))
  }

  const save = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!draft.name.trim()) {
      toast.error('Route name is required')
      return
    }
    if (draft.provider_ids.length === 0) {
      toast.error('Select at least one provider')
      return
    }

    if (isEditing) {
      await updateMutation.mutateAsync({
        path: { id: routeId },
        body: draft,
      })
    } else {
      await createMutation.mutateAsync({ body: draft })
    }
  }

  const isLoading = providersLoading || (isEditing && routeLoading)
  const isSaving = createMutation.isPending || updateMutation.isPending

  if (
    routeError ||
    (isEditing && (!Number.isInteger(routeId) || routeId <= 0))
  ) {
    return (
      <div className="container mx-auto max-w-4xl py-6">
        <EmptyState
          icon={RouteIcon}
          title="Notification route not found"
          description="The requested route may have been deleted or is no longer available."
          action={<Button onClick={returnToRoutes}>Back to Routes</Button>}
        />
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="container mx-auto max-w-5xl space-y-6 py-6">
        <div className="space-y-4">
          <Button variant="ghost" className="-ml-3" onClick={returnToRoutes}>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to Routes
          </Button>
          <div>
            <h1 className="text-3xl font-bold tracking-tight">
              {isEditing
                ? 'Edit Notification Route'
                : 'Create Notification Route'}
            </h1>
            <p className="mt-1 text-muted-foreground">
              Match a severity range to one or more notification destinations.
            </p>
          </div>
        </div>

        <form onSubmit={save}>
          <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_20rem]">
            <Card>
              <CardHeader>
                <CardTitle>Route configuration</CardTitle>
                <CardDescription>
                  Define when this route matches and where notifications go.
                </CardDescription>
              </CardHeader>
              <CardContent className="space-y-6">
                <div className="space-y-2">
                  <Label htmlFor="route-name">Name</Label>
                  <Input
                    id="route-name"
                    value={draft.name}
                    placeholder="Critical incidents"
                    disabled={isLoading}
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        name: event.target.value,
                      }))
                    }
                  />
                </div>

                <div className="space-y-3">
                  <Label>Severity range</Label>
                  <div className="grid gap-3 sm:grid-cols-2">
                    <div className="space-y-1.5">
                      <p className="text-xs text-muted-foreground">From</p>
                      <Select
                        value={draft.min_severity}
                        disabled={isLoading}
                        onValueChange={(min_severity) =>
                          setDraft((current) => ({
                            ...current,
                            min_severity,
                            max_severity:
                              severities.indexOf(min_severity) >
                              severities.indexOf(current.max_severity)
                                ? min_severity
                                : current.max_severity,
                          }))
                        }
                      >
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {severities.map((severity) => (
                            <SelectItem key={severity} value={severity}>
                              {severityLabels[severity]}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>
                    <div className="space-y-1.5">
                      <p className="text-xs text-muted-foreground">Through</p>
                      <Select
                        value={draft.max_severity}
                        disabled={isLoading}
                        onValueChange={(max_severity) =>
                          setDraft((current) => ({
                            ...current,
                            min_severity:
                              severities.indexOf(max_severity) <
                              severities.indexOf(current.min_severity)
                                ? max_severity
                                : current.min_severity,
                            max_severity,
                          }))
                        }
                      >
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {severities.map((severity) => (
                            <SelectItem key={severity} value={severity}>
                              {severityLabels[severity]}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>
                  </div>
                  <Badge variant="secondary">
                    {severityRangeLabel(draft.min_severity, draft.max_severity)}
                  </Badge>
                </div>

                <div className="space-y-3">
                  <div>
                    <Label>Destinations</Label>
                    <p className="mt-1 text-sm text-muted-foreground">
                      Create one Slack provider per webhook-bound channel, then
                      select the channel destinations for this route.
                    </p>
                  </div>
                  {!providersLoading && providers?.length === 0 ? (
                    <div className="rounded-lg border border-dashed p-6 text-center">
                      <BellRing className="mx-auto mb-3 h-6 w-6 text-muted-foreground" />
                      <p className="font-medium">No providers available</p>
                      <p className="mt-1 text-sm text-muted-foreground">
                        Add a destination before creating a route.
                      </p>
                      <Button
                        type="button"
                        variant="outline"
                        className="mt-4"
                        onClick={() => navigate('/settings/notifications/new')}
                      >
                        Add Provider
                      </Button>
                    </div>
                  ) : (
                    <div className="grid gap-3 sm:grid-cols-2">
                      {providers?.map((provider) => {
                        const selected = draft.provider_ids.includes(
                          provider.id
                        )
                        const channel = configuredSlackChannel(provider)
                        return (
                          <label
                            key={provider.id}
                            className="flex cursor-pointer items-start gap-3 rounded-lg border p-4 transition-colors hover:bg-muted/40"
                          >
                            <Checkbox
                              className="mt-0.5"
                              checked={selected}
                              disabled={isLoading}
                              onCheckedChange={(checked) =>
                                toggleProvider(provider.id, checked === true)
                              }
                            />
                            <span className="min-w-0 flex-1">
                              <span className="block truncate text-sm font-medium">
                                {provider.name}
                              </span>
                              <span className="mt-1 block text-xs capitalize text-muted-foreground">
                                {provider.provider_type}
                                {channel ? ` · ${channel}` : ''}
                              </span>
                            </span>
                          </label>
                        )
                      })}
                    </div>
                  )}
                </div>
              </CardContent>
            </Card>

            <div className="space-y-4">
              <Card>
                <CardHeader>
                  <CardTitle className="text-base">Delivery behavior</CardTitle>
                </CardHeader>
                <CardContent className="space-y-4">
                  <div className="flex items-center justify-between gap-4">
                    <div>
                      <Label htmlFor="route-enabled">Route enabled</Label>
                      <p className="mt-1 text-xs text-muted-foreground">
                        Disabled routes never deliver notifications.
                      </p>
                    </div>
                    <Switch
                      id="route-enabled"
                      checked={draft.enabled}
                      disabled={isLoading}
                      onCheckedChange={(enabled) =>
                        setDraft((current) => ({ ...current, enabled }))
                      }
                    />
                  </div>
                  <div className="rounded-lg bg-muted/50 p-4 text-sm text-muted-foreground">
                    Every matching route sends once to each selected provider.
                    For exclusive delivery, keep severity ranges non-overlapping
                    or narrow the all-severities Default route.
                  </div>
                  {draft.provider_ids.length > 0 && (
                    <div className="space-y-2">
                      <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                        Selected destinations
                      </p>
                      {draft.provider_ids.map((providerId) => (
                        <div key={providerId} className="text-sm">
                          {providerById.get(providerId)?.name ||
                            `Provider ${providerId}`}
                        </div>
                      ))}
                    </div>
                  )}
                </CardContent>
              </Card>

              <div className="flex gap-3 lg:flex-col-reverse">
                <Button
                  type="button"
                  variant="outline"
                  className="flex-1"
                  onClick={returnToRoutes}
                >
                  Cancel
                </Button>
                <Button
                  type="submit"
                  className="flex-1"
                  disabled={isLoading || isSaving}
                >
                  {isSaving
                    ? 'Saving…'
                    : isEditing
                      ? 'Save Changes'
                      : 'Create Route'}
                </Button>
              </div>
            </div>
          </div>
        </form>
      </div>
    </div>
  )
}
